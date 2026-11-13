//! The tier-0 bytecode interpreter.
//!
//! A closure body compiles to bytecode on its second call, or takes its code from its package's
//! code image then; the closures of the namespaces the start-up image holds arrive compiled. The
//! code replaces the body as a `BCODESXP`, as GNU R stores compiled closures, and everything that
//! reads a body as source (`body()`, `deparse`, `identical`, serialization) goes through
//! [`closure_body`], which returns the expression. Closures over one body expression share its
//! code (see [`Bodies`]), which lives as long as the expression does.
//!
//! The machine has registers instead of an operand stack: each instruction names the registers
//! it reads and writes, and the register file is a stack the collector traces. Every construct
//! the compiler inlines (`if`, `for`, `+`, `<-`, ...) runs only after a guard checks that its
//! symbol still resolves to the base primitive; when it does not, the whole expression runs on
//! the AST evaluator instead, so redefining `if` or `+` keeps GNU R's semantics. Variable and
//! function lookups carry inline caches, and arithmetic sites record the operand types they see
//! (type feedback), which is what the optimizing tier (the JIT in the `jit` crate) specializes on.
//!
//! A `BCODESXP` holds:
//! - `car`: the instruction words, an integer vector.
//! - `cdr`: the constants, a list. The first is the source expression, the second the cache
//!   stamps (a double vector holding each stamp's bits), the third the type feedback (a raw vector with one byte per
//!   instruction word, set at the position of the instruction that produced a value), the fourth
//!   the loop table (an integer vector of `start, end, exit, step` quadruples), the fifth the
//!   number of registers the code uses, the sixth the profile counters (an integer vector, see
//!   [`profile`]) the seventh the optimizing tier's compiled code, `NULL` until it exists, the eighth the
//!   slot hints (an integer vector, one per lookup site), the ninth the stored half of the
//!   effect-free fact (see [`crate::effects`]), the tenth the last argument-match plan of a
//!   closure body (see `eval::match_args_cached`), and the eleventh the slot table (see [`SLOTS`]).
//! - `tag`: the inline caches, a list with two slots per cache: the enclosure the lookup started
//!   from and the binding cell it found. The list holds them weakly, because closures that share
//!   a body share its code, and a strong cache would keep the last caller's environment alive.
//!
//! A closure's argument that is a call compiles too, into a code object of its own whose source
//! is the argument. A promise made from it keeps that code in its attribute slot, which a
//! promise otherwise never uses, while its code slot keeps the expression for `substitute`,
//! `PRCODE` and every other reader. Forcing the promise runs the code in the frame it was
//! compiled for, through the same lookups and caches as the body (see [`Compiler::call_site`]).
//!
//! The optimizing tier plugs in through [`Tier`]. It works on the same register file: a register
//! window is a fixed slice of one preallocated buffer that never moves, so compiled code keeps a
//! pointer to its window, stores its boxed values there (which roots them), and on
//! deoptimization returns [`TierExit::Resume`], and the interpreter continues at any
//! instruction.

use {
    crate::{
        Frame, Interp,
        arith::{MATHS, math_index},
        builtins::{Arg, BuiltinDef, Call, Generic, internal},
        conditions, dispatch, effects, element_access,
        envir::namespace_binding,
        eval::{self, Reify, SwitchSelector, as_condition},
        interp,
        kernels::{self, Element, MAX_NODES, Node, Reduction},
        moves,
        unwind::{Jump, R, Unwind, error, error_in},
        values::{self, length, missing_arg, nil, symbol_name, symbols, unbound},
    },
    heap::{
        Heap, Liveness, Marker,
        activity::{self, Activity, Counter},
        object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Sexp, SexpType, gp, is_na_real},
    },
    rustc_hash::{FxHashMap, FxHashSet},
    smallvec::{SmallVec, smallvec},
    thin_vec::ThinVec,
    std::{
        hash::{Hash, Hasher},
        ops::Range,
        rc::Rc,
    },
};

/// The internals behind GNU R's `compiler` package. Its code buffer is assembled in R, and
/// `mkCode` turns it into byte code; ry keeps only the buffer's first constant, the source
/// expression, exactly as it reads GNU R byte code from a stream, and compiles that itself.
pub const BUILTINS: &[BuiltinDef] = &[
    internal("bcVersion", 0, |_| unsafe {
        values::scalar_integer(GNU_R_BYTECODE_VERSION)
    }),
    internal("mkCode", 2, do_mk_code),
    internal("bcClose", 3, |call| unsafe {
        let closure = eval::make_closure(call.arg(0), call.arg(1), nil(), call.arg(2))?;
        crate::heap().root(closure);
        // `cmpfun` compiles at once, as GNU R's does, rather than on the second call: compiled
        // code passes constant arguments as values, which native code such as rlang observes.
        if worth_compiling(closure.cdr()) {
            let code = compile(closure.cdr(), closure, true)?;
            crate::heap().set_cdr(closure, code);
        }
        Ok(closure)
    }),
    internal("growconst", 1, do_grow_constants),
    internal("putconst", 3, do_put_constant),
    internal("getconst", 2, do_get_constants),
    internal("enableJIT", 1, |call| unsafe {
        let previous = interp().bytecode.jit_level;
        let level = values::as_integer_scalar(call.arg(0));
        if level >= 0 && level != NA_INTEGER {
            interp().bytecode.jit_level = level.min(3);
        }
        values::scalar_integer(previous)
    }),
    internal("compilePKGS", 1, |_| unsafe { values::scalar_logical(0) }),
];

/// The byte-code version GNU R 4.3 writes, which `compiler` stamps into every code buffer.
const GNU_R_BYTECODE_VERSION: i32 = 12;

/// The bytecode tier's session state: the register file and the call counts that decide when
/// a closure compiles.
pub struct State {
    /// Allocated once at [`REGISTER_FILE_CAPACITY`] and never grown past it, so a register
    /// window's address is stable for as long as the window exists.
    pub(crate) registers: Vec<Sexp>,
    /// The tier of every closure body that has run, shared by all closures over the body.
    bodies: Bodies,
    tier: Option<Tier>,
    pub(crate) call_plans: CallPlans,
    /// What the dispatch loop resumes as each callee it entered in place returns, innermost
    /// last ([`Resume`]).
    resumes: Vec<Resume>,
    /// The `for` loop states of the bodies entered in place. Allocated once at
    /// [`LOOP_STATE_CAPACITY`] and never grown past it, so a running body's states stay where
    /// they are while deeper calls take and return theirs.
    loop_states: Vec<LoopState>,
    /// GNU R's JIT level (`compiler::enableJIT`), 3 by default. It decides only what compiled
    /// code binds at compile time, since ry compiles at every level: at 0 GNU R compiles no
    /// closure, and below 3 no top-level loop, so such code guards its base functions as GNU R's
    /// evaluator would look them up.
    jit_level: i32,
}

impl Default for State {
    fn default() -> State {
        State {
            registers: Vec::with_capacity(REGISTER_FILE_CAPACITY),
            bodies: Bodies::default(),
            tier: None,
            call_plans: CallPlans::default(),
            resumes: Vec::new(),
            loop_states: Vec::with_capacity(LOOP_STATE_CAPACITY),
            jit_level: 3,
        }
    }
}

impl State {
    pub(crate) unsafe fn trace(&self, marker: &mut Marker<'_>) {
        for &register in &self.registers {
            marker.mark(register);
        }
        self.call_plans.trace(marker);
    }

    pub(crate) unsafe fn clear_dead(&mut self, liveness: &Liveness<'_>) {
        self.call_plans.clear_dead(liveness);
        self.bodies.clear_dead(liveness);
    }
}

/// The tier of each closure body, keyed by the body expression, which every closure made from
/// one `function` expression shares, and so does every copy `environment<-` makes, as R6 does
/// for each object's methods. Code compiled for one closure serves every other closure over the
/// body whose formals and srcref are the same objects. Guarded code reads nothing else of the
/// closure, since the enclosure only resolves names the code looks up at run time. Code that
/// binds its base functions also depends on which names the enclosure binds when it compiles, so
/// it serves a closure only under GNU R's `jit_env_match`: the closure's top environment is the
/// one the code compiled under, and every name its formals and its local frames up to that top
/// environment bind was bound when the code compiled. A closure that masks a base function
/// locally therefore never runs code that bound it; it compiles its own, guarded, as GNU R runs
/// such a closure uncompiled. A body counts
/// its calls across all its closures until it compiles, and then holds its code while any
/// closure runs it, so a closure made meanwhile runs the code from its first call instead of
/// compiling again.
///
/// GNU R shares compiled bodies the same way, through a cache keyed by a hash of the body's
/// structure. As there, a body changed in place since it compiled (through the C API) does not
/// give its old code to a new closure, while closures that already run the code keep it.
#[derive(Default)]
struct Bodies {
    /// Below [`COMPILED`], the calls the body has seen; with that bit set, the index of its
    /// compiled entry in the columns below.
    states: FxHashMap<Sexp, u32>,
    /// The compiled entries, as columns indexed by entry. `keys` holds the body, or `NULL` for
    /// a free entry. None of the columns is traced: an entry holds its objects weakly and goes
    /// once any of them dies, so the code lives exactly as long as a closure that runs it.
    keys: Vec<Sexp>,
    codes: Vec<Sexp>,
    formals: Vec<Sexp>,
    srcrefs: Vec<Sexp>,
    /// The [`shape`] of the body, formals and srcref when the code compiled.
    shapes: Vec<u64>,
    /// For code that binds its base functions, the top environment of the closure it compiled
    /// for, and `NULL` for guarded code. Namespaces and the global environment never die, but
    /// an environment that only binds `.packageName` can, so the entry goes with it.
    tops: Vec<Sexp>,
    /// For code that binds, the names the closure's formals and local frames bound when it
    /// compiled, sorted by address. Symbols are never collected, so the snapshot holds them
    /// without tracing them.
    locals: Vec<Box<[Sexp]>>,
    free: Vec<u32>,
}

/// The bit of a [`Bodies`] state that marks an entry index rather than a call count.
const COMPILED: u32 = 1 << 31;

/// What a closure over a body that has not compiled in it does at a call.
enum BodyTier {
    /// Runs the body's code, compiled for another closure over it.
    Shared(Sexp),
    /// Compiles now, binding its base functions when the flag is set: the body compiled for a
    /// closure this one cannot share code with.
    Compile(bool),
    /// Runs on the AST evaluator, after this many earlier calls.
    Counting(u32),
}

impl Bodies {
    /// The tier of `function`, a closure over `body`, installing shared code as its body.
    unsafe fn tier(&self, function: Sexp, body: Sexp) -> BodyTier {
        let Some(&state) = self.states.get(&body) else {
            return BodyTier::Counting(0);
        };
        if state & COMPILED == 0 {
            return BodyTier::Counting(state);
        }
        let entry = (state & !COMPILED) as usize;
        let formals = function.car();
        let srcref = values::get_attr(function, symbols().srcref);
        if formals != self.formals[entry]
            || srcref != self.srcrefs[entry]
            || shape(body, formals, srcref) != self.shapes[entry]
        {
            return BodyTier::Compile(binds_at_compile(function, body));
        }
        let binds = binds_at_compile(function, body);
        let top = self.tops[entry];
        let shares = if top == nil() {
            !binds
        } else {
            binds && binds_like(function, top, &self.locals[entry])
        };
        if !shares {
            // A closure that would bind but whose names differ runs guarded code, as GNU R's
            // JIT marks it never to compile.
            return BodyTier::Compile(binds && top == nil());
        }
        let code = self.codes[entry];
        crate::heap().set_cdr(function, code);
        BodyTier::Shared(code)
    }

    /// Counts a call of `body` on the AST evaluator.
    fn count(&mut self, body: Sexp, calls: u32) {
        if self.states.len() > MAX_TRACKED_BODIES {
            self.states.retain(|_, state| *state & COMPILED != 0);
        }
        self.states.insert(body, calls);
    }

    /// Records `code`, just compiled for `function`, as `body`'s code for closures to share. A
    /// body that already has an entry keeps it: the code there serves the closures it was
    /// compiled for, and replacing it would only make two kinds of closure take turns.
    unsafe fn share(&mut self, function: Sexp, body: Sexp, code: Sexp) {
        if self
            .states
            .get(&body)
            .is_some_and(|state| state & COMPILED != 0)
        {
            return;
        }
        let formals = function.car();
        let srcref = values::get_attr(function, symbols().srcref);
        let shape = shape(body, formals, srcref);
        let (top, locals) = if code.has_gp(gp::BINDING_CODE) {
            compile_environment(function)
        } else {
            (nil(), Box::default())
        };
        let entry = match self.free.pop() {
            Some(entry) => {
                let index = entry as usize;
                self.keys[index] = body;
                self.codes[index] = code;
                self.formals[index] = formals;
                self.srcrefs[index] = srcref;
                self.shapes[index] = shape;
                self.tops[index] = top;
                self.locals[index] = locals;
                entry
            }
            None => {
                self.keys.push(body);
                self.codes.push(code);
                self.formals.push(formals);
                self.srcrefs.push(srcref);
                self.shapes.push(shape);
                self.tops.push(top);
                self.locals.push(locals);
                (self.keys.len() - 1) as u32
            }
        };
        self.states.insert(body, entry | COMPILED);
    }

    /// Frees every entry that holds an object the collection found dead. The body may outlive
    /// its code, as a quoted `function` expression does, and it then counts its calls afresh.
    /// A dead body's call count goes too, before anything is freed, so a new body the allocator
    /// places at its address starts from zero.
    unsafe fn clear_dead(&mut self, liveness: &Liveness<'_>) {
        self.states
            .retain(|&body, state| *state & COMPILED != 0 || liveness.is_live(body));
        let live = |object: Sexp| object == nil() || liveness.is_live(object);
        for entry in 0..self.keys.len() {
            let body = self.keys[entry];
            if body == nil() {
                continue;
            }
            if !live(body)
                || !live(self.codes[entry])
                || !live(self.formals[entry])
                || !live(self.srcrefs[entry])
                || !live(self.tops[entry])
            {
                self.states.remove(&body);
                self.keys[entry] = nil();
                self.codes[entry] = nil();
                self.formals[entry] = nil();
                self.srcrefs[entry] = nil();
                self.tops[entry] = nil();
                self.locals[entry] = Box::default();
                self.free.push(entry as u32);
            }
        }
    }
}

/// The top environment of `function`'s enclosure and the names its formals and the frames from
/// the enclosure up to that top environment bind, sorted, which is what code that binds its base
/// functions depends on besides the body (GNU R's `make_cached_cmpenv`).
unsafe fn compile_environment(function: Sexp) -> (Sexp, Box<[Sexp]>) {
    let heap = crate::heap();
    let top = crate::dispatch::top_env(function.tag());
    let mut names = Vec::new();
    let mut formal = function.car();
    while formal != nil() {
        names.push(formal.tag());
        formal = formal.cdr();
    }
    let mut env = function.tag();
    while env != top && env != values::empty_env() {
        names.extend(heap.frame_symbols(env));
        env = env.enclosure();
    }
    names.sort_unstable_by_key(|name| name.as_ptr());
    names.dedup();
    (top, names.into_boxed_slice())
}

/// Whether code compiled under `top` with the local names `locals` serves `function`: its top
/// environment is `top`, and its formals and local frames bind only names in `locals` (GNU R's
/// `jit_env_match`). Compiling depends on which names are bound, never on their values.
unsafe fn binds_like(function: Sexp, top: Sexp, locals: &[Sexp]) -> bool {
    let env = function.tag();
    if crate::dispatch::top_env(env) != top {
        return false;
    }
    let mut formal = function.car();
    while formal != nil() {
        if !binds_name(locals, formal.tag()) {
            return false;
        }
        formal = formal.cdr();
    }
    let mut frame = env;
    while frame != top && frame != values::empty_env() {
        let bound = crate::heap().frame_symbols(frame);
        if bound.iter().any(|&name| !binds_name(locals, name)) {
            return false;
        }
        frame = frame.enclosure();
    }
    true
}

/// Whether the sorted snapshot `locals` holds `name`.
fn binds_name(locals: &[Sexp], name: Sexp) -> bool {
    locals
        .binary_search_by_key(&name.as_ptr(), |local| local.as_ptr())
        .is_ok()
}

/// A hash of what compiled code depends on: the structure of `body` and `formals`, and
/// `srcref`. Symbols, strings and lists hash by identity, and so does any constant longer than
/// one element; scalars hash by value, which is what an in-place change of a call reaches.
// Out of line so a profile shows what the reuse check costs.
#[inline(never)]
unsafe fn shape(body: Sexp, formals: Sexp, srcref: Sexp) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    hash_shape(body, &mut hasher);
    hash_shape(formals, &mut hasher);
    srcref.hash(&mut hasher);
    if srcref.kind() == SexpType::Integer {
        values::ints(srcref).hash(&mut hasher);
    }
    hasher.finish()
}

/// Walks each call's arguments in a loop and keeps only nested calls on an explicit stack,
/// because a long left-nested chain such as `a + b + c + ...` nests calls in the car as deep as
/// it is long.
unsafe fn hash_shape(expression: Sexp, hasher: &mut rustc_hash::FxHasher) {
    let mut pending: SmallVec<[Sexp; 16]> = smallvec![expression];
    while let Some(mut node) = pending.pop() {
        while matches!(node.kind(), SexpType::Language | SexpType::Pairlist) {
            (node.kind() as u8).hash(hasher);
            node.tag().hash(hasher);
            let head = node.car();
            if matches!(head.kind(), SexpType::Language | SexpType::Pairlist) {
                pending.push(head);
            } else {
                hash_leaf(head, hasher);
            }
            node = node.cdr();
        }
        hash_leaf(node, hasher);
    }
}

unsafe fn hash_leaf(node: Sexp, hasher: &mut rustc_hash::FxHasher) {
    match node.kind() {
        // Reading an ALTREP vector can run R code, so one hashes by identity.
        _ if node.is_altrep() => node.hash(hasher),
        SexpType::Integer | SexpType::Logical if length(node) == 1 => {
            values::ints(node).hash(hasher);
        }
        SexpType::Real if length(node) == 1 => {
            values::reals(node)[0].to_bits().hash(hasher);
        }
        _ => node.hash(hasher),
    }
}

/// The register file holds this many registers across all active frames. Deep recursion runs
/// out of C stack or `expressions` long before it runs out of registers.
const REGISTER_FILE_CAPACITY: usize = 1 << 21;

/// How many `for` loop states the bodies entered in place hold between them. A call whose body
/// would need more runs through [`run_at`], which keeps its states on the Rust stack.
const LOOP_STATE_CAPACITY: usize = 1 << 14;

/// A threshold no counter is compared against. Counters saturate at `i32::MAX`, so a
/// threshold of that value would otherwise fire once a counter saturates.
pub const NEVER: i32 = i32::MAX;

/// The optimizing tier's entry points.
#[derive(Clone, Copy)]
pub struct Tier {
    /// Called when compiled bytecode starts running with a fresh register window, if the code
    /// has optimized code already, is on its first entry, or has been entered
    /// [`Tier::entry_threshold`] times. Returns `None` to let the interpreter run it from the
    /// start.
    pub enter: unsafe fn(&mut Machine<'_>) -> Option<TierExit>,
    /// Entries after which a body without optimized code asks [`Tier::enter`] to compile it.
    pub entry_threshold: i32,
    /// Called when a body without optimized code reaches the entry that starts its recording
    /// window. Returns `false` to skip recording because the body will never compile, in which
    /// case it also marks the body refused.
    pub worth_recording: unsafe fn(&Machine<'_>) -> bool,
    /// Called on a backward jump to `target` once the code's back-edge counter crosses
    /// [`Tier::back_edge_threshold`]. Returns `None` to keep interpreting at `target`.
    pub back_edge: unsafe fn(&mut Machine<'_>, target: usize) -> Option<TierExit>,
    pub back_edge_threshold: i32,
    /// Called before a typed closure gets a frame, with its arguments matched by position or by
    /// exact name. Returns `None` to let the call proceed as usual, or the value a native
    /// function computed without a frame, promises or annotation checks.
    pub call: unsafe fn(&TypedCall<'_>) -> Option<R<Sexp>>,
}

/// A call of a typed closure that [`Tier::call`] may run natively. Native code has no frame, so
/// it pushes one from these only around an R call it makes.
#[derive(Clone, Copy)]
pub struct TypedCall<'a> {
    pub call: Sexp,
    pub function: Sexp,
    pub args: &'a [crate::builtins::Arg],
    /// The environment the call was made from, or `NULL` for a slot frame's caller, as
    /// [`crate::interp::Frame::sys_parent`] holds it.
    pub sys_parent: Sexp,
}

/// How the optimizing tier hands an activation back to the interpreter.
pub enum TierExit {
    /// The body ran to the end with this result.
    Finished(R<Sexp>),
    /// The optimizing tier gave the activation up at `pc`, with the registers and loop states as
    /// tier 0 expects them there. Interpretation continues at `pc`, handling `pending` as if the
    /// instruction at `pc` had raised it. The optimizing tier returns this instead of
    /// interpreting from inside its own frame, so a loop that leaves compiled code and enters it
    /// again does not nest native frames.
    Resume { pc: usize, pending: Option<Unwind> },
}

/// One activation of compiled bytecode as the interpreter sees it: what [`Tier`] receives.
pub struct Machine<'a> {
    /// The closure whose body `code` is, or `NULL` for a promise's code.
    pub function: Sexp,
    pub code: Sexp,
    /// The register that holds the activation's environment, just below its slots and
    /// registers. It holds `NULL` while the activation runs in a slot frame that has none yet
    /// (`docs/src/content/docs/development/design/slot-frames.md`).
    pub header: usize,
    /// Where the activation's register window starts in the register file.
    pub base: usize,
    /// The handle-stack mark to release to on a backward jump.
    pub mark: usize,
    pub loops: &'a mut [LoopState],
}

impl Machine<'_> {
    /// The activation's environment, or `NULL` while its slot frame has none.
    #[inline(always)]
    pub unsafe fn env(&self) -> Sexp {
        *register_address(self.header)
    }

    /// The activation's environment, made from its slots first if it has none.
    #[inline]
    pub unsafe fn reify(&self, cause: Reify) -> R<Sexp> {
        let env = self.env();
        if env != nil() {
            return Ok(env);
        }
        eval::slot_frame_env(self.header, cause)
    }

    /// The environment free names are looked up in: the enclosure of the activation's frame.
    /// A slot frame's enclosure is its closure's, so asking for it does not reify.
    #[inline]
    pub unsafe fn enclosure(&self) -> Sexp {
        let env = self.env();
        if env != nil() {
            return env.enclosure();
        }
        self.function.tag()
    }

    /// The frame slot that holds `symbol`, while the activation runs in a slot frame without an
    /// environment. `None` with an environment, which is then the source of truth, and for a
    /// symbol the slot table does not number, which the frame cannot bind.
    pub unsafe fn slot_of(&self, symbol: Sexp) -> Option<*mut Sexp> {
        if self.env() != nil() {
            return None;
        }
        let slots = self.code.cdr().element(SLOTS);
        if slots == nil() {
            return None;
        }
        let names = slots.element(1);
        (0..length(names))
            .find(|&index| names.element(index) == symbol)
            .and_then(|position| self.frame_slot(position))
    }

    /// The frame slot at `position` in the slot table, while the activation runs in a slot frame
    /// without an environment: [`Machine::slot_of`] for a caller that numbered its symbols ahead.
    #[inline(always)]
    pub unsafe fn frame_slot(&self, position: usize) -> Option<*mut Sexp> {
        (self.env() == nil()).then(|| register_address(self.header + 1 + position))
    }

    /// What the activation's own frame binds `symbol` to, without forcing or reifying: a slot's
    /// content or a binding cell's value. `None` when the frame does not bind it.
    pub unsafe fn local_binding(&self, symbol: Sexp) -> Option<Sexp> {
        let env = self.env();
        if env != nil() {
            return crate::heap().frame_cell(env, symbol).map(|cell| cell.cdr());
        }
        self.slot_of(symbol)
            .map(|slot| *slot)
            .filter(|&value| value != unbound())
    }

    /// Binds `symbol` to `value` in the activation's own frame, as `SetVar` does: into its slot
    /// in a slot frame, clearing a formal's missing bit, and otherwise into the environment.
    /// `slot` is [`Machine::slot_of`] the symbol.
    pub unsafe fn assign(&self, slot: Option<*mut Sexp>, symbol: Sexp, value: Sexp) -> R<()> {
        if let Some(slot) = slot {
            // As `SetVar`'s fast handler stores, leaving an old value a reconcile could reclaim
            // to the collector rather than noting it now.
            let heap = crate::heap();
            if !heap.try_store_slot(&mut *slot, value) {
                heap.store_slot(&mut *slot, value);
            }
            self.clear_missing(slot);
            return Ok(());
        }
        let env = self.reify(Reify::Site("assignment"))?;
        eval::assign_symbol(symbol, value, env, false)
    }

    /// Assigns the scalar `element` of `kind` to `symbol` by storing it into the value the
    /// variable is bound to, when that is a plain scalar of `kind` that nothing else references.
    /// Returns that value, or `None` when the assignment needs a new one.
    /// `slot` is [`Machine::slot_of`] the symbol.
    pub unsafe fn overwrite(
        &self,
        slot: Option<*mut Sexp>,
        symbol: Sexp,
        kind: SexpType,
        element: f64,
    ) -> Option<Sexp> {
        let current = match slot {
            Some(slot) => {
                let current = *slot;
                if !overwrite_scalar(current, kind, element) {
                    return None;
                }
                self.clear_missing(slot);
                return Some(current);
            }
            None if self.env() == values::nil() => return None,
            None => crate::heap()
                .frame_cell(self.env(), symbol)
                .filter(|cell| !cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED))?
                .cdr(),
        };
        overwrite_scalar(current, kind, element).then_some(current)
    }

    unsafe fn clear_missing(&self, slot: *mut Sexp) {
        let formal = slot.offset_from(register_address(self.header)) as usize - 1;
        if formal < 64
            && let Some(frame) = interp().frames.last_mut()
        {
            frame.missing &= !(1 << formal);
        }
    }
}

/// Installs the optimizing tier.
pub fn set_tier(tier: Option<Tier>) {
    interp().bytecode.tier = tier;
}

pub fn tier() -> Option<&'static Tier> {
    interp().bytecode.tier.as_ref()
}

/// The address of register `index` of the file. Valid while the window that holds it exists.
#[inline]
pub fn register_address(index: usize) -> *mut Sexp {
    // The buffer never reallocates, so this pointer stays valid as the file grows and shrinks.
    unsafe { interp().bytecode.registers.as_mut_ptr().add(index) }
}

/// Grows the register file to `length` registers, filling new ones with `NULL`.
#[inline]
pub fn reserve_registers(length: usize) -> R<()> {
    if length <= interp().bytecode.registers.len() {
        return Ok(());
    }
    grow_registers(length)
}

#[inline(never)]
fn grow_registers(length: usize) -> R<()> {
    let registers = &mut interp().bytecode.registers;
    if length > registers.capacity() {
        return Err(error(
            "evaluation nested too deeply: the register file is full",
        ));
    }
    let start = registers.len();
    if length > start {
        // Every call grows the file past the registers the last call released, so this runs
        // once per call: `fill` stores the `NULL`s as wide writes, where `resize` cloned them
        // one at a time. Clearing is what lets a collection inside the new window free what a
        // returned call left in these words, as GNU R does. The collector marks the whole file
        // up to its length, not each window up to the registers it wrote, so a word left
        // uncleared would keep a returned call's temporary alive until the new window writes
        // it, and a finalizer GNU R runs in that collection would run late
        // (`gc__register_window_frees_returned_temporaries`).
        // SAFETY: the capacity covers `length`, and `Sexp` is a plain pointer the next line
        // initializes before anything reads it.
        unsafe { registers.set_len(length) };
        registers[start..length].fill(nil());
    }
    Ok(())
}

/// Runs a byte-code object that appears as an expression, which only happens when code reads a
/// compiled body directly and evaluates it.
pub unsafe fn execute(code: Sexp, env: Sexp) -> R<Sexp> {
    eval::eval(source_of(code), env)
}

/// Runs a closure's body, compiling it when [`schedule`] says to, or running the code another
/// closure over the same body compiled.
pub unsafe fn run_body(function: Sexp, body: Sexp, env: Sexp) -> R<Sexp> {
    if body.kind() == SexpType::Bytecode {
        return run(function, body, env);
    }
    if !worth_compiling(body) {
        return eval::eval(body, env);
    }
    let bodies = &mut interp().bytecode.bodies;
    let binds = match bodies.tier(function, body) {
        BodyTier::Shared(code) => return run(function, code, env),
        BodyTier::Compile(binds) => binds,
        BodyTier::Counting(calls) => match schedule(function, body, calls, true) {
            Some(binds) => binds,
            None => {
                bodies.count(body, calls + 1);
                return eval::eval(body, env);
            }
        },
    };
    let code = compile_for(function, body, binds)?;
    run(function, code, env)
}

/// Whether `function`'s body `body`, after `calls` earlier calls on the AST evaluator, compiles
/// at this call, and if so whether the code binds its base functions at compile time (see
/// [`Compiler::binding`]). Where GNU R's JIT would compile (`R_CheckJIT`), the code binds, at the
/// call where GNU R compiles: a closure over the global environment at its first call when its
/// `JIT_score` reaches [`MIN_JIT_SCORE`] and at its second otherwise, and any other closure at its
/// second call when its score reaches the minimum. A namespace's closure binds too, since GNU R
/// installs packages compiled, and compiles at its first call when hot and its second otherwise.
/// Every other closure, which GNU R never compiles, compiles on its [`COMPILE_AFTER_CALLS`]th
/// call with guarded base functions, so it looks each name up as it runs as GNU R's AST evaluator
/// does, and a typed one compiles guarded at its first call. `mark` sets the closure's
/// `MAYBE_JIT` and `NO_JIT` bits as GNU R's check does; without it this only reports what the
/// call would do.
unsafe fn schedule(function: Sexp, body: Sexp, calls: u32, mark: bool) -> Option<bool> {
    let due = calls + 1 >= COMPILE_AFTER_CALLS;
    if crate::annotations::is_typed(function) {
        return Some(false);
    }
    if function.has_gp(gp::CLOSURE_NO_JIT) || interp().bytecode.jit_level == 0 {
        return due.then_some(false);
    }
    if function.has_gp(gp::CLOSURE_MAYBE_JIT) {
        if mark {
            function.set_gp_bits(gp::CLOSURE_MAYBE_JIT, false);
        }
        return Some(true);
    }
    let enclosure = function.tag();
    let global = enclosure == crate::heap().known().global_env;
    let hot = jit_score(body) >= MIN_JIT_SCORE;
    if !global && crate::envir::is_namespace(crate::dispatch::top_env(enclosure)) {
        return (hot || due).then_some(true);
    }
    if global && hot {
        return Some(true);
    }
    if global || hot {
        if mark {
            function.set_gp_bits(gp::CLOSURE_MAYBE_JIT, true);
        }
        return None;
    }
    if mark {
        function.set_gp_bits(gp::CLOSURE_NO_JIT, true);
    }
    due.then_some(false)
}

/// Whether code compiled for `function` binds its base functions, which is whether GNU R would
/// compile the closure at all. [`schedule`] decides when it compiles; this reads only the
/// closure's bits and its body's score, never a call count, so a closure that has not reached
/// its compiling call can still take code another closure compiled.
unsafe fn binds_at_compile(function: Sexp, body: Sexp) -> bool {
    if crate::annotations::is_typed(function)
        || function.has_gp(gp::CLOSURE_NO_JIT)
        || interp().bytecode.jit_level == 0
    {
        return false;
    }
    let enclosure = function.tag();
    function.has_gp(gp::CLOSURE_MAYBE_JIT)
        || enclosure == crate::heap().known().global_env
        || crate::envir::is_namespace(crate::dispatch::top_env(enclosure))
        || jit_score(body) >= MIN_JIT_SCORE
}

/// Why a closure body runs on the AST evaluator rather than as compiled code. It is a property
/// of the body, not a failure: the compiler takes every call, sending a construct it does not
/// compile (such as a `break` outside a loop) to the AST evaluator from inside the code, so a
/// compile that fails is an error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Uncompilable {
    /// The body is a constant or a symbol, which evaluating costs less than entering code.
    NotACall,
}

/// Runs `call`, a `for`, `while` or `repeat` loop the tree walker was about to evaluate in `env`,
/// as compiled code, or returns `None` when the loop stays with the tree walker. The loop compiles
/// before its first iteration, as GNU R's JIT compiles every top-level loop when it starts, and
/// only in the global environment, as GNU R's does, which is where they are not inside a function
/// whose frame semantics (`on.exit`, `missing`, `sys.function`) the compiled loop would have
/// to reproduce. `sequence` is a `for` loop's evaluated sequence, which the compiled loop steps
/// through instead of evaluating the expression again.
///
/// The code compiles for this one evaluation and is dropped after it, as in GNU R, which also
/// retries a loop it could not compile at each evaluation. It runs on `env`'s bindings, so code
/// called from the loop sees every assignment at once, and in an activation with no call frame,
/// so `parent.frame()`, `sys.call()` and the calls conditions report are those of the tree
/// walker.
pub unsafe fn run_loop(call: Sexp, sequence: Option<Sexp>, env: Sexp) -> R<Option<Sexp>> {
    let heap = crate::heap();
    // A `return` would end the compiled activation as if it were a function, where the tree
    // walker raises "no function to return from".
    if env != heap.known().global_env || interp().debugger.is_some() || calls_return(call) {
        return Ok(None);
    }
    let body = match sequence {
        Some(sequence) => {
            let arguments = call.cdr();
            let rest = values::cons(sequence, arguments.cdr().cdr())?;
            values::lcons(call.car(), values::cons(arguments.car(), rest)?)?
        }
        None => call,
    };
    let function = values::triple(SexpType::Closure, nil(), body, env)?;
    // GNU R compiles a top-level loop when it starts, binding the base functions the global
    // environment finds in base then, except those the loop itself assigns (`compile` adds the
    // expression's `findLocals` as a frame).
    let binding = if interp().bytecode.jit_level >= 3 {
        Some(vec![Rc::new(compiler_locals(nil(), body, env)?)])
    } else {
        None
    };
    let code = compile_with_locals(body, function, None, binding)?;
    heap.set_cdr(function, code);
    let profile = values::ints_mut(code.cdr().element(PROFILE));
    profile[profile::TIER_STATE] = profile::TIER_OFF;
    profile[profile::NEXT] = i32::MAX;
    // A closure's frame restores the source reference on leaving, and this activation has none.
    let srcref = interp().srcref;
    let result = run(function, code, env);
    interp().srcref = srcref;
    result?;
    interp().visible = false;
    Ok(Some(nil()))
}

/// Whether `expression` calls `return` outside the functions it defines.
unsafe fn calls_return(expression: Sexp) -> bool {
    if expression.kind() != SexpType::Language {
        return false;
    }
    let head = expression.car();
    if head == symbols().function {
        return false;
    }
    if head.kind() == SexpType::Symbol && &*symbol_name(head) == "return" {
        return true;
    }
    let mut cell = expression;
    while matches!(cell.kind(), SexpType::Language | SexpType::Pairlist) {
        if calls_return(cell.car()) {
            return true;
        }
        cell = cell.cdr();
    }
    false
}

/// `function`'s body, compiled first when [`run_body`] would compile it at this call. Binding the
/// arguments calls this, so that such a call runs in a slot frame rather than in the environment
/// the uncompiled body needs: the first call of a body with a loop, which compiles at once, is
/// often its only call, as for a benchmark's driver. It counts nothing, so [`run_body`] counts a
/// call that does not compile.
pub unsafe fn compile_before_call(function: Sexp) -> R<Sexp> {
    let body = function.cdr();
    if !worth_compiling(body) {
        return Ok(body);
    }
    let binds = match interp().bytecode.bodies.tier(function, body) {
        BodyTier::Shared(code) => return Ok(code),
        BodyTier::Compile(binds) => binds,
        BodyTier::Counting(calls) => match schedule(function, body, calls, false) {
            Some(binds) => binds,
            None => return Ok(body),
        },
    };
    compile_for(function, body, binds)
}

/// Compiles `body` as `function`'s, binding its base functions when `binds`, or takes its code
/// from the package's code image, installs the code as the closure's body and records it for
/// other closures over the body.
unsafe fn compile_for(function: Sexp, body: Sexp, binds: bool) -> R<Sexp> {
    // A package's code image replaces compiling, at the same call, rather than running code from
    // the first call: decoding costs about as much as compiling, and many closures run only once.
    let code = match crate::code_image::take_pending(function, body)? {
        Some(code) => code,
        None => {
            let code = compile(body, function, binds)?;
            crate::heap().set_cdr(function, code);
            code
        }
    };
    interp().bytecode.bodies.share(function, body, code);
    Ok(code)
}

/// A closure over `env` for the `function` expression with `formals`, `body` and `srcref` in
/// the code `outer`, made as [`Op::Closure`] makes it: every closure made from the expression
/// shares one compiled body, kept in `outer`'s constants. A native prologue that runs part of
/// a compiled body by hand makes its closures with this, since a closure made from the source
/// alone would be compiled again for each call of the outer function.
pub(crate) unsafe fn inner_closure(
    outer: Sexp,
    formals: Sexp,
    body: Sexp,
    srcref: Sexp,
    env: Sexp,
) -> R<Sexp> {
    let heap = crate::heap();
    let site = if outer.kind() == SexpType::Bytecode {
        let constants = outer.cdr();
        (0..length(constants)).find_map(|position| {
            let constant = constants.element(position);
            let matches = constant == body
                || (constant.kind() == SexpType::Bytecode && source_of(constant) == body);
            matches.then_some((constants, position, constant))
        })
    } else {
        None
    };
    let Some((constants, position, shared)) = site else {
        return eval::make_closure(formals, body, srcref, env);
    };
    let closure = eval::make_closure(formals, shared, srcref, env)?;
    if shared.kind() != SexpType::Bytecode && worth_compiling(shared) {
        heap.root(closure);
        // Code that binds its base functions compiled its nested functions with it, so this
        // outer code guards them, and binding here would miss its locals; a namespace's code
        // binds anyway, as its package installs compiled.
        let binds = crate::envir::is_namespace(crate::dispatch::top_env(env))
            && env != heap.known().global_env;
        let code = compile(shared, closure, binds)?;
        heap.set_element(constants, position, code);
        heap.set_cdr(closure, code);
    }
    Ok(closure)
}

/// A closure's body as source: the expression of compiled code.
pub unsafe fn closure_body(function: Sexp) -> Sexp {
    let body = function.cdr();
    if body.kind() == SexpType::Bytecode {
        return source_of(body);
    }
    body
}

/// A closure's body compiled to byte code, compiling it now if it has not run often enough to
/// compile yet, or why the body stays an expression.
pub unsafe fn compiled_body(function: Sexp) -> R<Result<Sexp, Uncompilable>> {
    let body = function.cdr();
    if body.kind() == SexpType::Bytecode {
        return Ok(Ok(body));
    }
    if !worth_compiling(body) {
        return Ok(Err(Uncompilable::NotACall));
    }
    let binds = match interp().bytecode.bodies.tier(function, body) {
        BodyTier::Shared(code) => return Ok(Ok(code)),
        BodyTier::Compile(binds) => binds,
        BodyTier::Counting(calls) => schedule(function, body, calls, false).unwrap_or(false),
    };
    compile_for(function, body, binds).map(Ok)
}

/// The body a copy of the closure `function` gets: the code `function` runs, or the code
/// closures over its body share, so that the original and its copies run one code object, with
/// one profile and one tier-1 compilation. R6 copies every method into each object it creates,
/// and a replacement call such as `environment(f) <- env` duplicates `f` first. A body that has
/// not compiled is copied as the expression, and the copies count their calls together.
pub unsafe fn body_to_copy(function: Sexp) -> R<Sexp> {
    let body = function.cdr();
    if body.kind() == SexpType::Bytecode || !worth_compiling(body) {
        return Ok(body);
    }
    match interp().bytecode.bodies.tier(function, body) {
        BodyTier::Shared(code) => Ok(code),
        BodyTier::Compile(_) | BodyTier::Counting(_) => Ok(body),
    }
}

/// Compiles every closure bound in a loaded namespace afresh, for the start-up image, and returns
/// the new code objects in the order they were compiled. Code that start-up ran carries this
/// process's caches, feedback and profile, so the image keeps only these fresh objects, which no
/// call has touched yet, and stores any other code as its source. A start from the image then
/// runs base R's functions compiled from their first call.
pub unsafe fn compile_namespaces_for_image() -> R<Vec<Sexp>> {
    let heap = crate::heap();
    let registry = heap.known().namespace_registry;
    let mut namespaces = vec![heap.known().base_namespace];
    for symbol in crate::envir::env_symbols(registry, true, false) {
        if let Some(cell) = heap.frame_cell(registry, symbol)
            && cell.cdr().kind() == SexpType::Environment
            && !namespaces.contains(&cell.cdr())
        {
            namespaces.push(cell.cdr());
        }
    }
    let mut closures: Vec<Sexp> = Vec::new();
    for namespace in namespaces {
        for symbol in crate::envir::env_symbols(namespace, true, false) {
            let Some(cell) = heap.frame_cell(namespace, symbol) else {
                continue;
            };
            if cell.has_gp(gp::ACTIVE_BINDING) {
                continue;
            }
            // A lazy-load promise start-up never forced stays unforced, and its closure
            // compiles when a session first calls it.
            let function = forced_value(cell.cdr());
            if function.kind() != SexpType::Closure {
                continue;
            }
            closures.push(function);
        }
    }
    closures.sort_unstable_by_key(|function| function.as_ptr() as usize);
    closures.dedup();
    let mut fresh = Vec::new();
    for function in closures {
        let body = closure_body(function);
        if !worth_compiling(body) {
            continue;
        }
        // An error is the compiler's way of rejecting a body (a `break` outside a loop, say), so
        // such a body keeps running on the AST evaluator, as in `run_body`.
        if let Ok(code) = compile(body, function, true) {
            heap.set_cdr(function, code);
            fresh.push(code);
        }
    }
    Ok(fresh)
}

/// Compiles, ahead of their first call, the functions `code` defines with `function(...)`
/// expressions, as [`Op::Closure`] would at the first run of each, and the functions those
/// define in turn. `env` stands in for the frame such a function closes over, which compiling
/// reads only to resolve names it then guards at run time, so any frame below the enclosure gives
/// correct code. Installing a package calls this, so that a package's nested functions (R6
/// methods, callbacks) are written with its top-level ones.
///
/// # Safety
/// `code` is a live code object made by this module.
pub unsafe fn compile_nested_functions(code: Sexp, env: Sexp) -> R<()> {
    let words = values::ints(code.car()).to_vec();
    let constants = code.cdr();
    let mut pc = 0;
    while pc < words.len() {
        let word = words[pc] as u32;
        let Some(op) = op_of(word) else {
            return Err(error("compiled code holds an invalid instruction"));
        };
        let operand = |index: usize| words.get(pc + 1 + index).map(|&word| word as usize);
        if op == Op::Closure
            && let (Some(formals), Some(body), Some(srcref)) = (operand(1), operand(2), operand(3))
            && body < length(constants)
        {
            let source = constants.element(body);
            let nested = if source.kind() == SexpType::Bytecode {
                Some(source)
            } else if worth_compiling(source) {
                let closure = eval::make_closure(
                    constants.element(formals),
                    source,
                    constants.element(srcref),
                    env,
                )?;
                crate::heap().root(closure);
                // A nested function the outer code had not compiled with it guards its base
                // functions, since `env` does not hold the outer function's locals.
                let compiled = compile(source, closure, false)?;
                crate::heap().set_element(constants, body, compiled);
                Some(compiled)
            } else {
                None
            };
            if let Some(nested) = nested {
                compile_nested_functions(nested, env)?;
            }
        }
        pc += 1 + operand_count(op);
    }
    Ok(())
}

pub unsafe fn is_compiled(function: Sexp) -> bool {
    function.cdr().kind() == SexpType::Bytecode
}

/// Whether a register of a running compiled body may hold `value`. A register that held it
/// for an expression that finished still matches, which only costs an update in place.
pub fn registers_hold(value: Sexp) -> bool {
    interp().bytecode.registers.contains(&value)
}

const COMPILE_AFTER_CALLS: u32 = 2;
const MAX_TRACKED_BODIES: usize = 1 << 16;

pub const SOURCE: usize = 0;
pub const EPOCHS: usize = 1;
pub const FEEDBACK: usize = 2;
pub const LOOPS: usize = 3;
pub const REGISTERS: usize = 4;
pub const PROFILE: usize = 5;
pub const TIER1: usize = 6;
pub const HINTS: usize = 7;
pub const SUMMARY: usize = 8;
pub const MATCHES: usize = 9;
/// `NULL`, or for a closure body whose call frames keep their variables in slots, a list of the
/// slot table (per inline cache, the slot of the symbol a `GetVar`, `SetVar` or `GetFun` with
/// that cache reads, or -1) and the slots' symbols, the formals first in order.
pub const SLOTS: usize = 10;
pub const FIRST_CONSTANT: usize = 11;

/// The most formals a slot frame takes: [`crate::Frame::missing`] has a bit per formal.
pub const MAX_SLOT_FORMALS: usize = 64;

/// The profile counters, positions in the `PROFILE` integer vector.
pub mod profile {
    /// How often the code started running.
    pub const ENTRIES: usize = 0;
    /// Backward jumps taken since the counter was last reset.
    pub const BACK_EDGES: usize = 1;
    /// The optimizing tier's state for this code, which only that tier interprets, apart from
    /// [`TIER_OFF`].
    pub const TIER_STATE: usize = 2;
    /// The [`TIER_STATE`] of a loop compiled for the tree walker ([`super::run_loop`]), which no
    /// tier compiles or records feedback for: it runs without a call frame, and the tiers' frames
    /// and their resume points assume one. Negative, so the optimizing tier treats it as refused.
    pub const TIER_OFF: i32 = i32::MIN;
    /// Nonzero while tier 0 records type feedback for this code, which only happens with the
    /// optimizing tier installed and while the code has no optimized code: during the first
    /// activation of a body with a loop, over the last eighth of the entry threshold
    /// ([`super::recording_start`]), from half the back-edge threshold, and after its optimized
    /// code was discarded until it compiles again. Recording costs a type test and a store per
    /// value, and most bodies never get near a threshold, so recording from the first entry
    /// made every call of cold package code slower with the optimizing tier installed than
    /// without it. Recording from half the entry threshold still cost a test suite 10M
    /// instructions, in warm bodies that never reach it; a type the window misses costs one
    /// deoptimization and a recompile. A loop can turn hot inside a body's only activation,
    /// so a loop that reaches half its threshold starts recording in the running activation, and
    /// the first activation records the instructions before the loop, which that would miss.
    /// Optimized code records nothing, and what tier 0 would record beside it is read only by a
    /// recompile, which follows a discard that turns recording back on.
    pub const RECORDING: usize = 3;
    /// The entry count at which the next entry of this code asks [`super::entry_event`] what to
    /// do, or `i32::MIN` to ask at the very next entry. Every tier decision at entry (recording,
    /// the first-call compile, the entry-threshold compile, running optimized
    /// code) happens at a known count, so one comparison per call stands in for testing each
    /// of them, which cost about 30 instructions per call of every body. Whatever changes a
    /// count or a state those decisions read outside an entry sets this to `i32::MIN`.
    pub const NEXT: usize = 4;
    /// The session's call clock ([`crate::interp::Interp::calls_ever`]) when the recording window
    /// opened, which the optimizing tier reads to estimate how often the body
    /// runs.
    pub const WINDOW: usize = 5;
    pub const LENGTH: usize = 6;
    /// The counters a code image keeps. The next event is derived from them when it is read.
    pub const SERIALIZED: usize = 4;
}

/// The body `function` was defined with, which a compiled closure keeps beside its code.
pub unsafe fn source_body(function: Sexp) -> Sexp {
    if is_compiled(function) {
        source_of(function.cdr())
    } else {
        function.cdr()
    }
}

/// The source of a closure body, compiled or not.
pub unsafe fn source_of_body(body: Sexp) -> Sexp {
    if body.kind() == SexpType::Bytecode {
        source_of(body)
    } else {
        body
    }
}

pub unsafe fn source_of(code: Sexp) -> Sexp {
    code.cdr().element(SOURCE)
}

const LOOP_SEARCH_DEPTH: u32 = 32;

/// The site a call records where it emits no guard, because the code binds the base function
/// the guard would have checked.
const UNGUARDED: u32 = u32::MAX;

/// GNU R's `JIT_score` of `expression`, capped at [`MIN_JIT_SCORE`]: a loop scores the minimum,
/// any other call one plus its arguments' scores, and an `if` the larger of its condition's and
/// its consequent's, which leaves out the alternative exactly as GNU R does.
unsafe fn jit_score(expression: Sexp) -> u32 {
    if expression.kind() != SexpType::Language {
        return 1;
    }
    let head = expression.car();
    if head.kind() == SexpType::Symbol {
        match &*symbol_name(head) {
            "if" => {
                let arguments = expression.cdr();
                let condition = jit_score(arguments.car());
                let consequent = jit_score(arguments.cdr().car());
                return condition.max(consequent);
            }
            "for" | "while" | "repeat" => return MIN_JIT_SCORE,
            _ => {}
        }
    }
    let mut score = 1;
    let mut cell = expression.cdr();
    while matches!(cell.kind(), SexpType::Pairlist | SexpType::Language) && score < MIN_JIT_SCORE {
        score += jit_score(cell.car());
        cell = cell.cdr();
    }
    score.min(MIN_JIT_SCORE)
}

/// GNU R's `MIN_JIT_SCORE`, which is also the score of a loop.
const MIN_JIT_SCORE: u32 = 50;

/// Where GNU R's compiler finds a name's binding, by its `frameTypes`: a frame below the code's
/// top environment, a namespace frame (the namespace, its imports and the base namespace), or
/// the global environment and what is attached to it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Local,
    Namespace,
    Global,
}

/// The frame that binds `symbol` first, looking outward from `enclosure`, and its kind. Any
/// binding counts, a variable as much as a function, as for GNU R's `exists`.
unsafe fn compile_time_frame(symbol: Sexp, enclosure: Sexp) -> Option<(Sexp, FrameKind)> {
    let heap = crate::heap();
    let top = crate::dispatch::top_env(enclosure);
    let in_namespace = crate::envir::is_namespace(top);
    let mut kind = FrameKind::Local;
    let mut env = enclosure;
    while env != heap.known().empty_env {
        if env == top {
            kind = if in_namespace {
                FrameKind::Namespace
            } else {
                FrameKind::Global
            };
        }
        if env == heap.known().global_env {
            kind = FrameKind::Global;
        }
        if heap.frame_cell(env, symbol).is_some() {
            return Some((env, kind));
        }
        env = env.enclosure();
    }
    None
}

/// The base function GNU R's compiler binds `symbol` to when it compiles code that closes over
/// `enclosure`, or `None` when it compiles a call of `symbol` that looks the function up as the
/// code runs (`getInlineInfo` with no run-time guard). Outside a namespace that is a name of
/// [`LANGUAGE_FUNCTIONS`] whose first binding is base's; inside one, any name with an inline
/// handler that the namespace's frames find in base, which the namespace cannot rebind.
unsafe fn base_binding_at_compile(symbol: Sexp, enclosure: Sexp) -> Option<Sexp> {
    let heap = crate::heap();
    let name = symbol_name(symbol);
    if INLINE_HANDLERS.binary_search(&&*name).is_err() || &*name == "standardGeneric" {
        return None;
    }
    let (frame, kind) = compile_time_frame(symbol, enclosure)?;
    let in_base = frame == heap.known().base_env || frame == heap.known().base_namespace;
    let bound = match kind {
        FrameKind::Local => false,
        FrameKind::Namespace => in_base,
        FrameKind::Global => {
            frame == heap.known().base_env && LANGUAGE_FUNCTIONS.binary_search(&&*name).is_ok()
        }
    };
    if !bound {
        return None;
    }
    let value = forced_value(heap.frame_cell(frame, symbol)?.cdr());
    values::is_function(value).then_some(value)
}

/// The names GNU R's compiler takes as a function's locals (`funEnv`): its formals, and every
/// variable that its formals' defaults and its body assign with `<-` or `=`, bind with `for`, or
/// name as a literal string in a two-argument `assign` or `delayedAssign`. The search skips
/// nested functions and formulas, and skips `local`, `quote` and `expression` calls unless the
/// function rebinds those names or they are not base's (`findLocalsList`), which outside a
/// namespace holds for `quote` and `expression` always.
unsafe fn compiler_locals(formals: Sexp, body: Sexp, enclosure: Sexp) -> R<FxHashSet<Sexp>> {
    let quoting = ["expression", "local", "quote"];
    let mut symbols_of: SmallVec<[Sexp; 3]> = SmallVec::new();
    for name in quoting {
        symbols_of.push(values::install(name)?);
    }
    let unbound: SmallVec<[&str; 3]> = quoting
        .into_iter()
        .zip(symbols_of.iter())
        .filter(|(_, symbol)| !is_base_variable(**symbol, enclosure))
        .map(|(name, _)| name)
        .collect();
    let mut shadowed: SmallVec<[&str; 3]> = quoting.into_iter().collect();
    loop {
        let mut quoted = false;
        let found = assigned_names(formals, body, &shadowed, &mut quoted)?;
        let previous = shadowed.len();
        shadowed = quoting
            .into_iter()
            .zip(symbols_of.iter())
            .filter(|(name, symbol)| {
                unbound.contains(name) || (shadowed.contains(name) && found.contains(*symbol))
            })
            .map(|(name, _)| name)
            .collect();
        // GNU R stops when the count of shadowed names repeats, not the names themselves.
        // Without a `local`, `quote` or `expression` call the next pass would find the same.
        if shadowed.len() == previous || !quoted {
            let mut locals = found;
            let mut formal = formals;
            while formal.kind() == SexpType::Pairlist {
                locals.insert(formal.tag());
                formal = formal.cdr();
            }
            return Ok(locals);
        }
    }
}

/// GNU R's `isBaseVar`: whether the compiler finds `symbol`'s binding in base and may inline it.
unsafe fn is_base_variable(symbol: Sexp, enclosure: Sexp) -> bool {
    let heap = crate::heap();
    let name = symbol_name(symbol);
    match compile_time_frame(symbol, enclosure) {
        Some((frame, FrameKind::Namespace)) => {
            frame == heap.known().base_env || frame == heap.known().base_namespace
        }
        Some((frame, FrameKind::Global)) => {
            frame == heap.known().base_env && LANGUAGE_FUNCTIONS.binary_search(&&*name).is_ok()
        }
        _ => false,
    }
}

/// One pass of GNU R's `findLocalsList1` over a function's formals' defaults and body, with the
/// names in `shadowed` searched inside. Sets `quoting` when it meets a call of one of those
/// names, without which the result does not depend on `shadowed`.
unsafe fn assigned_names(
    formals: Sexp,
    body: Sexp,
    shadowed: &[&str],
    quoting: &mut bool,
) -> R<FxHashSet<Sexp>> {
    let mut found = FxHashSet::default();
    let mut todo: Vec<Sexp> = Vec::new();
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        todo.push(formal.car());
        formal = formal.cdr();
    }
    todo.push(body);
    while let Some(expression) = todo.pop() {
        if expression.kind() != SexpType::Language {
            continue;
        }
        let head = expression.car();
        let arguments = expression.cdr();
        let name = match head.kind() {
            SexpType::Symbol => Some(symbol_name(head)),
            SexpType::String if values::length(head) >= 1 => values::first_string(head),
            _ => None,
        };
        let Some(name) = name else {
            todo.push(head);
            push_items(arguments, &mut todo);
            continue;
        };
        match &*name {
            "=" | "<-" => {
                if let Some(symbol) = assigned_variable(arguments.car()) {
                    found.insert(symbol);
                }
                push_items(arguments, &mut todo);
            }
            "for" => {
                if arguments.car().kind() == SexpType::Symbol {
                    found.insert(arguments.car());
                }
                push_items(arguments.cdr(), &mut todo);
            }
            "assign" | "delayedAssign" => {
                let target = arguments.car();
                if values::pairlist_length(arguments) == 2
                    && target.kind() == SexpType::String
                    && values::length(target) == 1
                    && let Some(text) = values::first_string(target)
                {
                    found.insert(values::install(&text)?);
                    todo.push(arguments.cdr().car());
                }
            }
            "function" | "~" => {}
            "local" => {
                *quoting = true;
                if shadowed.contains(&"local") || values::pairlist_length(arguments) != 1 {
                    push_items(arguments, &mut todo);
                }
            }
            "expression" | "quote" => {
                *quoting = true;
                if shadowed.contains(&&*name) {
                    push_items(arguments, &mut todo);
                }
            }
            _ => push_items(arguments, &mut todo),
        }
    }
    Ok(found)
}

unsafe fn push_items(mut cell: Sexp, todo: &mut Vec<Sexp>) {
    while matches!(cell.kind(), SexpType::Pairlist | SexpType::Language) {
        todo.push(cell.car());
        cell = cell.cdr();
    }
}

/// GNU R's `getAssignedVar`: the variable an assignment to `target` binds, the innermost first
/// argument of a replacement such as `names(x)[2]`.
unsafe fn assigned_variable(mut target: Sexp) -> Option<Sexp> {
    while target.kind() == SexpType::Language {
        target = target.cdr().car();
    }
    match target.kind() {
        SexpType::Symbol if target != missing_arg() => Some(target),
        SexpType::String if values::length(target) >= 1 => {
            values::install(&values::first_string(target)?).ok()
        }
        _ => None,
    }
}

/// Whether GNU R's compiler, having found `name` bound in base, compiles `call` with its inline
/// handler rather than as a call that looks the function up when it runs. Every handler for the
/// names [`LANGUAGE_FUNCTIONS`] lists accepts its call, or falls back to calling base's
/// function directly, except these.
unsafe fn handler_binds(name: &str, call: Sexp, enclosure: Sexp) -> bool {
    match name {
        // `cmpMultiColon` compiles a call that finds `::` when it runs.
        "::" | ":::" => false,
        "local" => values::pairlist_length(call.cdr()) == 1,
        "function" => !may_call_browser(call.cdr().cdr().car(), enclosure),
        _ => true,
    }
}

/// GNU R's `mayCallBrowser`: whether `expression` calls `browser`, outside the functions it
/// defines.
unsafe fn may_call_browser(expression: Sexp, enclosure: Sexp) -> bool {
    if expression.kind() != SexpType::Language {
        return false;
    }
    let head = expression.car();
    if head.kind() == SexpType::Symbol {
        let name = symbol_name(head);
        if &*name == "browser" {
            return true;
        }
        if &*name == "function" && is_base_variable(head, enclosure) {
            return false;
        }
    } else if may_call_browser(head, enclosure) {
        return true;
    }
    let mut cell = expression.cdr();
    while matches!(cell.kind(), SexpType::Pairlist | SexpType::Language) {
        if may_call_browser(cell.car(), enclosure) {
            return true;
        }
        cell = cell.cdr();
    }
    false
}

/// The base functions GNU R's compiler has an inline handler for, in byte order.
#[rustfmt::skip]
const INLINE_HANDLERS: [&str; 233] = [
    "!", "!=", "$", "$<-", "%%", "%*%", "%/%", "&", "&&", "(", "*", "+", "-", "...elt",
    "...length", "...names", ".C", ".Call", ".Call.graphics", ".External", ".External.graphics",
    ".External2", ".Fortran", ".Internal", ".Primitive", ".cache_class", ".class2",
    ".isMethodsDispatchOn", ".primTrace", ".primUntrace", ".subset", ".subset2", "/", ":", "::",
    ":::", "<", "<-", "<<-", "<=", "=", "==", ">", ">=", "@", "@<-", "Arg", "Conj", "Exec", "Im",
    "Mod", "Re", "Tailcall", "UseMethod", "[", "[<-", "[[", "[[<-", "^", "abs", "acos", "acosh",
    "all", "any", "anyNA", "as.call", "as.character", "as.complex", "as.double", "as.environment",
    "as.integer", "as.logical", "as.numeric", "as.raw", "asin", "asinh", "atan", "atan2", "atanh",
    "attr", "attr<-", "attributes", "attributes<-", "baseenv", "besselY", "beta", "break",
    "browser", "c", "call", "ceiling", "choose", "class", "class<-", "cos", "cosh", "cospi",
    "crossprod", "cummax", "cummin", "cumprod", "cumsum", "declare", "digamma", "dim", "dim<-",
    "dimnames", "dimnames<-", "drop", "emptyenv", "enc2native", "enc2utf8", "environment<-", "exp",
    "expm1", "expression", "floor", "for", "forceAndCall", "function", "gamma", "gc.time",
    "globalenv", "identical", "if", "inherits", "interactive", "invisible", "is.array",
    "is.atomic", "is.call", "is.character", "is.complex", "is.double", "is.environment",
    "is.expression", "is.finite", "is.function", "is.infinite", "is.integer", "is.language",
    "is.list", "is.loaded", "is.logical", "is.matrix", "is.na", "is.name", "is.nan", "is.null",
    "is.numeric", "is.object", "is.pairlist", "is.raw", "is.recursive", "is.single", "is.symbol",
    "is.vector", "isS4", "lazyLoadDBfetch", "lbeta", "lchoose", "length", "length<-", "levels<-",
    "lgamma", "list", "local", "log", "log10", "log1p", "log2", "match", "max", "min", "missing",
    "names", "names<-", "nargs", "nchar", "next", "nzchar", "oldClass", "oldClass<-", "on.exit",
    "polyroot", "pos.to.env", "proc.time", "prod", "quote", "range", "rep", "rep.int", "rep_len",
    "repeat", "require", "retracemem", "return", "round", "seq.int", "seq_along", "seq_len",
    "sign", "signif", "sin", "sinh", "sinpi", "sqrt", "standardGeneric", "storage.mode<-",
    "substitute", "sum", "switch", "tan", "tanh", "tanpi", "tcrossprod", "tracemem", "trigamma",
    "trunc", "typeof", "unCfillPOSIXlt", "unclass", "untracemem", "vector", "which.max",
    "which.min", "while", "with", "xtfrm", "{", "|", "||", "~",
];

/// GNU R's `languageFuns`, in byte order: the inline handlers its compiler applies, at the
/// default optimization level, to code outside a namespace without a run-time check.
#[rustfmt::skip]
const LANGUAGE_FUNCTIONS: [&str; 46] = [
    "!", "!=", "$", "$<-", "%%", "%*%", "%/%", "&", "&&", "(", "*", "+", "-", "/", ":", "::",
    ":::", "<", "<-", "<<-", "<=", "=", "==", ">", ">=", "@", "@<-", "[", "[<-", "[[", "[[<-", "^",
    "break", "for", "function", "if", "local", "next", "repeat", "return", "switch", "while", "{",
    "|", "||", "~",
];

/// The compile-time frames of promise code made for `function`'s code ([`Compiler::binding`]):
/// none when that code guards its base functions, and otherwise the function's locals. Promise
/// code compiles when its call site first runs rather than with the function, so it binds what
/// the names find then.
unsafe fn promise_binding(function: Sexp) -> R<Option<Vec<Rc<FxHashSet<Sexp>>>>> {
    let code = function.cdr();
    if code.kind() != SexpType::Bytecode || !code.has_gp(gp::BINDING_CODE) {
        return Ok(None);
    }
    let locals = compiler_locals(function.car(), source_of(code), function.tag())?;
    Ok(Some(vec![Rc::new(locals)]))
}

/// The arguments a closure called from `function`'s code makes its promises from: each one
/// worth compiling replaced by a code object for `function`'s frame, or `NULL` when there is no
/// function to compile against (the call is in promise code, whose frame is not known here).
unsafe fn compile_promise_arguments(function: Sexp, arguments: Sexp) -> R<Sexp> {
    if function == nil() {
        return Ok(nil());
    }
    let mut formals = FxHashSet::default();
    let mut formal = function.car();
    while formal.kind() == SexpType::Pairlist {
        formals.insert(formal.tag());
        formal = formal.cdr();
    }
    let mut compiler = Compiler {
        local_symbols: local_symbols(source_of_body(function.cdr()), function.car()),
        formals,
        code: Vec::new(),
        constants: Vec::new(),
        caches: 0,
        loops: Vec::new(),
        open_loops: Vec::new(),
        registers: 0,
        next_register: 0,
        enclosure: function.tag(),
        // `run_promise` restores the srcref that was current when forcing began, so the code
        // never needs one of its own to return to.
        srcref: nil(),
        rebinding: None,
        assigning: None,
        tail_updates: (Vec::new(), nil()),
        argument: true,
        summary: nil(),
        // Promise code runs on the slots of the frame that made it, so it shares that frame's
        // slot table instead of building its own.
        slot_formals: slot_names(function.cdr()),
        slots_fixed: true,
        bound: None,
        cold: Vec::new(),
        temporary: None,
        sites: None,
        hoisted: Vec::new(),
        preguarded: Vec::new(),
        binding: None,
        deferred_binding: function,
    };
    let items = values::pairlist_items(arguments);
    let heap = crate::heap();
    let mut list = nil();
    for (tag, value) in items.iter().rev() {
        let value = if compiles_as_promise(*value) {
            compiler.code.clear();
            compiler.constants.clear();
            compiler.caches = 0;
            compiler.loops.clear();
            compiler.open_loops.clear();
            compiler.registers = 0;
            compiler.next_register = 0;
            compiler.cold.clear();
            // An argument the compiler rejects still runs, as a promise of its expression.
            compiler.assemble(*value).unwrap_or(*value)
        } else {
            *value
        };
        heap.root(value);
        list = values::cons(value, list)?;
        heap.set_tag(list, *tag);
        heap.root(list);
    }
    Ok(list)
}

/// The promise code of `function`'s formal defaults over its slot frame, one element per formal:
/// a code object compiled against the body's slot table, a symbol, or `NULL` when the default
/// is a literal or cannot run on the slots (so a call that leaves that formal out needs a heap
/// frame), or `NULL` itself when there is nowhere to keep the list. `function`'s body must be
/// compiled with a slot table. The list is built on the first
/// call that needs it and kept in the table with the formals it was built for, because a closure
/// whose formals were replaced keeps the body's code.
pub unsafe fn slot_defaults(function: Sexp) -> R<Sexp> {
    let table = function.cdr().cdr().element(SLOTS);
    let formals = function.car();
    let kept = table.element(2);
    if kept != nil() && kept.car() == formals {
        return Ok(kept.cdr());
    }
    let compiled = compile_promise_arguments(function, formals)?;
    let heap = crate::heap();
    heap.root(compiled);
    let defaults = values::alloc(SexpType::List, length(formals))?;
    heap.root(defaults);
    for (index, (_, default)) in values::pairlist_items(compiled).iter().enumerate() {
        let fits = match default.kind() {
            SexpType::Bytecode => default.cdr().element(SLOTS) != nil(),
            SexpType::Symbol => {
                *default != symbols().dots
                    && *default != missing_arg()
                    && eval::dot_dot_index(*default).is_none()
            }
            _ => false,
        };
        if fits {
            heap.set_element(defaults, index, *default);
        }
    }
    let kept = values::cons(formals, defaults)?;
    heap.set_element(table, 2, kept);
    Ok(defaults)
}

/// The slot table names of `body`, when it is compiled with one.
unsafe fn slot_names(body: Sexp) -> Option<Vec<Sexp>> {
    if body.kind() != SexpType::Bytecode {
        return None;
    }
    let table = body.cdr().element(SLOTS);
    if table == nil() {
        return None;
    }
    let names = table.element(1);
    Some(
        (0..length(names))
            .map(|index| names.element(index))
            .collect(),
    )
}

/// Whether a call's argument becomes a promise of compiled code: a call, not a symbol or a
/// constant, whose evaluation needs nothing of the code that made it. `break`, `next` and
/// `return` act on the activation that evaluates them, which for a promise is not the one that
/// made it, and a loop is left out so that promise code never runs a back edge. A function
/// definition nested in a larger argument is left out too, but one that is the whole argument
/// (`vapply(x, function(v) ...)`) compiles: its [`Op::Closure`] then gives every closure the
/// argument makes one shared compiled body, where closures the AST evaluator makes would each
/// compile their own, and so would each tier up on their own.
unsafe fn compiles_as_promise(expression: Sexp) -> bool {
    unsafe fn free_of_control(expression: Sexp, depth: u32) -> bool {
        match expression.kind() {
            SexpType::Symbol => !matches!(
                &*symbol_name(expression),
                "break" | "next" | "return" | "function" | "for" | "while" | "repeat"
            ),
            SexpType::Language => {
                if depth == 0 {
                    return false;
                }
                let mut cell = expression;
                while matches!(cell.kind(), SexpType::Language | SexpType::Pairlist) {
                    if !free_of_control(cell.car(), depth - 1) {
                        return false;
                    }
                    cell = cell.cdr();
                }
                true
            }
            _ => true,
        }
    }
    expression.kind() == SexpType::Language
        && (expression.car() == symbols().function
            || free_of_control(expression, LOOP_SEARCH_DEPTH))
}

/// A body is worth compiling when it does more than evaluate one constant or symbol.
unsafe fn worth_compiling(body: Sexp) -> bool {
    body.kind() == SexpType::Language
}

// Instructions. Each opcode is followed by its operands; registers, constant indices, cache
// indices and jump targets are all word indices.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Op {
    /// `dst constant`
    Constant,
    /// `dst symbol cache`
    GetVar,
    /// `symbol src`: a local `<-`.
    SetVar,
    /// `dst symbol cache`
    GetFun,
    /// `function target`: jumps unless the function is a builtin (whose arguments are values).
    BranchUnlessBuiltin,
    /// `dst function call base count`: calls a builtin with the values in registers
    /// `base..base + count`, tagged as in `call`.
    CallBuiltin,
    /// `dst function call base count target`: [`Op::CallBuiltin`] for `target <- f(target, ...)`
    /// with `f` a builtin that may update its first argument in place, which it may when
    /// `target`'s binding holds the value's only reference.
    CallBuiltinRebinding,
    /// `dst function call`: any other call, with the AST evaluator's argument handling. The
    /// constant after `call` holds the arguments a closure's promises are made from
    /// ([`Compiler::call_site`]).
    CallGeneric,
    /// `dst expression`: evaluates an expression the compiler did not compile.
    Ast,
    /// `symbol expected cache probe target`: jumps unless `symbol` names the function `expected`.
    /// `probe` is 0 when the frame cannot bind `symbol`, so only the cache is checked.
    Guard,
    /// `target`
    Jump,
    /// `src call target`: jumps when the condition is false.
    JumpUnless,
    /// `operator dst left right call function reuse`. Bit 0 of `reuse` marks `left` and bit 1
    /// `right` as a temporary that only this instruction reads, so a plain vector there with no
    /// counted reference may hold the result (`arithmetic_destination`).
    Arithmetic,
    /// `src`
    Return,
    /// `loop src call`: prepares the sequence in `src` for iteration.
    ForInit,
    /// `loop src symbol target`: binds the next element, or jumps to `target` when done.
    ForStep,
    /// `src`: the loop over the sequence in `src` is done.
    ForEnd,
    Invisible,
    Visible,
    /// `dst src call is_and target`: the left operand of `&&` or `||`; jumps to `target` with
    /// the result in `dst` when it decides the result.
    LogicLeft,
    /// `dst left src call is_and`
    LogicRight,
    /// `dst formals body srcref`
    Closure,
    /// `dst variable index value call kind`: `variable[index] <- value`, `[[` or `$` as the
    /// [`ElementKind`] operand says, where `index` is a register that already holds the index's
    /// value (the name as a string for `$`) and the assignment runs as `<-` would run it.
    SubAssign,
    /// `srcref statement`: makes the srcref constant current, for a statement of a `{` with
    /// source references; `statement` is 1 when a statement starts there, which an attached
    /// debugger may stop on, and 0 when a block's end restores the outer srcref.
    SetSrcref,
    /// `src expected target`: jumps unless the value in `src` is the constant `expected` itself,
    /// compared by identity. A call of a base function tests that the name still finds the
    /// primitive `expected`, and a compiled `switch` tests the arm [`Op::SwitchArm`] chose.
    BranchUnlessSame,
    /// `src target`: jumps when the value in `src` is an object (it has a class).
    BranchIfObject,
    /// `dst function call subject`: calls a dispatch-or-eval special such as `[` whose first
    /// argument already evaluated to the object in `subject`, so it dispatches on that object
    /// with the other arguments as promises.
    DispatchSubject,
    /// `dst start end`: ends a statement of a `{`. It clears `dst` and the statement's
    /// temporaries `start..end` and releases the activation's handles, because the register
    /// file and the handle stack are roots and would otherwise keep what the statement dropped
    /// alive until the closure returns, so a `gc()` in the next statement could not finalize it.
    EndStatement,
    /// `dst function call target`: [`Op::CallGeneric`] for the right-hand side of
    /// `target <- f(target, ...)`, which may update the value of `target` in place
    /// (`eval::rebind_call`).
    CallRebinding,
    /// `dst function call target`: the body's last call, `f(target, ...)` for a library builtin
    /// that updates its first argument in place, where `target` is the first parameter. It may
    /// update the parameter's value in place while a move into this frame lasts
    /// (`eval::tail_update`).
    CallTailUpdate,
    /// `dst src name call shape`: `src$name` for the base `$`. `shape` is a constant integer
    /// vector caching the last record layout the site saw and the field's slot in it, so a
    /// record's field is one comparison and a slot read; anything else takes `$`'s own path.
    Field,
    /// `dst src index shape target`: `src[[index]]` for a constant `index`, through `shape` as
    /// for [`Op::Field`], when `src` is a plain record or tuple with that item; otherwise it
    /// jumps to `target`, where `[[`'s own path runs.
    Item,
    /// `src layout target`: jumps unless `src` is a plain record, tuple or tag of `layout`, the
    /// test of a compiled pattern whose syntax fixes the layout.
    MatchShape,
    /// `src tag target`: jumps unless `src` is a plain tag named `tag`, whatever its payload.
    MatchTag,
    /// `src pattern target`: jumps unless `src` matches the literal constant `pattern`.
    MatchLiteral,
    /// `dst src index layout`: slot `index` of `src`, which a [`Op::MatchShape`] showed has
    /// `layout`.
    Slot,
    /// `src call`: no arm of a compiled `match` matched `src`.
    MatchFail,
    /// `dst layout base count`: the record, tuple or tag of `layout` whose slots are in registers
    /// `base..base + count`.
    Construct,
    /// `dst src`: copies a register.
    Move,
    /// `dst expression names base count`: evaluates `expression` in a new child of the frame
    /// where each symbol of the list `names` is bound to the value in the matching register of
    /// `base..base + count`. A `match` guard's code runs this way wherever it cannot read the
    /// bound names from registers.
    BoundEval,
    /// `dst src guard call`: sets `dst` to `TRUE` or `FALSE` as the value in `src` of the `match`
    /// guard `guard` holds or not. Errors name `call`, the `match`.
    MatchCondition,
    /// `dst src table`: the arm of a compiled `switch` that the selector in `src` chooses, as
    /// the integer scalar `table` holds for it, or the scalar 0 when no arm runs (see
    /// [`switch_table`]). The arms follow as a chain of [`Op::BranchUnlessSame`] tests, which
    /// compare that scalar by identity, so choosing an arm costs one pointer compare per arm.
    SwitchArm,
    /// `dst variable row column value call`: `variable[row, column] <- value` or
    /// `variable[[row, column]] <- value`, as `call` says, with the subscripts' values in
    /// registers `row` and `column`, run as `<-` would run it.
    SubAssignMatrix,
    /// `dst variable setter cache call base count whole`: `whole`, an assignment such as
    /// `names(x) <- value`, through the replacement function `setter` names (looked up through
    /// lookup slot `cache`). Registers `base + 1..base + count` hold the other arguments and the
    /// value last; the instruction puts the variable's value in `base`, calls the replacement as
    /// `call`, and binds the result to the variable. What [`replace_variable`] does not handle
    /// runs `whole` as `<-` would.
    Replace,
    /// `target`: a guard fallback laid out after the body returning to it. Unlike a backward
    /// `Jump`, it is never a loop's back edge.
    Resume,
    /// `dst symbol`: `missing(symbol)` for a plain symbol, read from the frame without building
    /// the special's call.
    Missing,
    /// `dst package symbol exported cache`: `package::symbol` (`exported` 1) or
    /// `package:::symbol` (0) for literal names, read from the namespace binding the cache slot
    /// holds while the lookup stamps of the bindings that led to it stand still
    /// ([`namespace_binding`]).
    Namespace,
    /// `dst program target`: an element-wise expression such as `exp(a * b + c)` or
    /// `sum(x > 0)` computed in one pass by `kernels::fused`, without intermediate vectors,
    /// after which execution goes on at `target`. The expression's ordinary code follows, and
    /// runs whenever the fused pass declines. `program` is the integer vector [`fusion`] lays
    /// out, or `NULL` when the ordinary code did not take the expected shape.
    Fused,
    /// `dst program target`: an [`Op::Fused`] site that gave up, which does nothing. The
    /// compiler emits it when the ordinary code did not take the expected shape, and a fused
    /// site rewrites its own opcode to it after [`fusion::GIVE_UP`] declines in a row, so a site
    /// that never fuses costs one dispatch in tier 0.
    Unfused,
    /// `symbol src`: `symbol <<- src`, which assigns in the first enclosing frame that binds
    /// `symbol`, or in the global environment.
    SuperAssign,
    /// `dst function call`: the fast-path prologue `.Internal(call)` that opens a body
    /// (`base_fast_paths.rs`), calling the internal `function`. When the prologue finishes the
    /// call, the activation returns its value; otherwise `dst` gets `NULL` and the R body runs.
    Prologue,
    /// `dst call internal first count`: a call of a base closure whose body is only
    /// `.Internal(f(...))`, inlined as GNU R's compiler inlines its "safe base internals": the
    /// internal's arguments, evaluated in the caller, are in registers `first..first + count`,
    /// and the builtin `internal` runs on them with `call` as the call its errors name.
    Internal,
}

/// Which replacement an [`Op::SubAssign`] does, stored as its sixth operand.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ElementKind {
    /// `x[i] <- value`
    Single = 0,
    /// `x[[i]] <- value`
    Double = 1,
    /// `x$name <- value`
    Dollar = 2,
}

pub const OPS: [Op; 52] = [
    Op::Constant,
    Op::GetVar,
    Op::SetVar,
    Op::GetFun,
    Op::BranchUnlessBuiltin,
    Op::CallBuiltin,
    Op::CallBuiltinRebinding,
    Op::CallGeneric,
    Op::Ast,
    Op::Guard,
    Op::Jump,
    Op::JumpUnless,
    Op::Arithmetic,
    Op::Return,
    Op::ForInit,
    Op::ForStep,
    Op::ForEnd,
    Op::Invisible,
    Op::Visible,
    Op::LogicLeft,
    Op::LogicRight,
    Op::Closure,
    Op::SubAssign,
    Op::SetSrcref,
    Op::BranchUnlessSame,
    Op::BranchIfObject,
    Op::DispatchSubject,
    Op::EndStatement,
    Op::CallRebinding,
    Op::CallTailUpdate,
    Op::Field,
    Op::Item,
    Op::MatchShape,
    Op::MatchTag,
    Op::MatchLiteral,
    Op::Slot,
    Op::MatchFail,
    Op::Construct,
    Op::Move,
    Op::BoundEval,
    Op::MatchCondition,
    Op::SwitchArm,
    Op::SubAssignMatrix,
    Op::Replace,
    Op::Resume,
    Op::Missing,
    Op::Namespace,
    Op::Fused,
    Op::Unfused,
    Op::SuperAssign,
    Op::Prologue,
    Op::Internal,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Arith {
    Add,
    Subtract,
    Multiply,
    Divide,
    Power,
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Modulo,
    IntegerDivide,
}

pub const ARITHS: [(Arith, &str); 13] = [
    (Arith::Add, "+"),
    (Arith::Subtract, "-"),
    (Arith::Multiply, "*"),
    (Arith::Divide, "/"),
    (Arith::Power, "^"),
    (Arith::Equal, "=="),
    (Arith::NotEqual, "!="),
    (Arith::Less, "<"),
    (Arith::Greater, ">"),
    (Arith::LessEqual, "<="),
    (Arith::GreaterEqual, ">="),
    (Arith::Modulo, "%%"),
    (Arith::IntegerDivide, "%/%"),
];

/// Type feedback: the kinds of value an instruction produced, one bit per kind. A scalar is a
/// length-one vector and a plain vector is any other; both have no attributes and are not
/// ALTREP. An Array of logical, integer or double items is its own kind at every length, because
/// it promises what a plain vector does not (no NA, no attributes) and its operators differ. A
/// compact integer or double sequence (`1:n`, `as.double(1:n)`) is its own kind too, because
/// compiled code computes its elements instead of loading them.
pub mod feedback {
    pub const INTEGER_SCALAR: u32 = 1 << 0;
    pub const DOUBLE_SCALAR: u32 = 1 << 1;
    pub const LOGICAL_SCALAR: u32 = 1 << 2;
    pub const INTEGER_VECTOR: u32 = 1 << 3;
    pub const DOUBLE_VECTOR: u32 = 1 << 4;
    pub const LOGICAL_VECTOR: u32 = 1 << 5;
    pub const LIST: u32 = 1 << 6;
    pub const OTHER: u32 = 1 << 7;
    pub const INTEGER_ARRAY: u32 = 1 << 8;
    pub const DOUBLE_ARRAY: u32 = 1 << 9;
    pub const LOGICAL_ARRAY: u32 = 1 << 10;
    /// A `JumpUnless` jumped: its condition was `FALSE`.
    pub const BRANCH_TAKEN: u32 = 1 << 11;
    /// A `JumpUnless` fell through: its condition was `TRUE`.
    pub const BRANCH_NOT_TAKEN: u32 = 1 << 12;
    /// A `GetVar` read an active binding, so every read there must call the binding again.
    /// Compiled code records it, and the bit says nothing about the value's type.
    pub const ACTIVE_BINDING: u32 = 1 << 13;
    /// An attribute-free compact integer sequence.
    pub const INTEGER_SEQUENCE: u32 = 1 << 15;
    /// An attribute-free compact double sequence, such as `as.double(1:n)`.
    pub const DOUBLE_SEQUENCE: u32 = 1 << 16;
}

/// The feedback tier 0 recorded at `pc` of the byte code `code`: the [`feedback`] kinds of the
/// values a value-producing instruction made, or the branch bits of a `JumpUnless`. Tier 0 only
/// adds bits, so what it reports never shrinks until the optimizing tier clears the record.
pub unsafe fn feedback_bits(code: Sexp, pc: usize) -> u32 {
    values::ints(code.cdr().element(FEEDBACK))
        .get(pc)
        .map_or(0, |&bits| bits as u32)
}

/// The [`feedback`] bit for `value`.
pub unsafe fn value_kind(value: Sexp) -> u32 {
    if value.is_altrep() {
        let Some([integer, double]) = crate::compact_sequence_classes() else {
            return feedback::OTHER;
        };
        let class = value.tag();
        return if values::has_attributes(value) {
            feedback::OTHER
        } else if class == integer {
            feedback::INTEGER_SEQUENCE
        } else if class == double {
            feedback::DOUBLE_SEQUENCE
        } else {
            feedback::OTHER
        };
    }
    if value.is_array() {
        return match value.kind() {
            SexpType::Integer => feedback::INTEGER_ARRAY,
            SexpType::Real => feedback::DOUBLE_ARRAY,
            SexpType::Logical => feedback::LOGICAL_ARRAY,
            _ => feedback::OTHER,
        };
    }
    if values::has_attributes(value) {
        return feedback::OTHER;
    }
    let scalar = value.length() == 1;
    match (value.kind(), scalar) {
        (SexpType::Integer, true) => feedback::INTEGER_SCALAR,
        (SexpType::Real, true) => feedback::DOUBLE_SCALAR,
        (SexpType::Logical, true) => feedback::LOGICAL_SCALAR,
        (SexpType::Integer, false) => feedback::INTEGER_VECTOR,
        (SexpType::Real, false) => feedback::DOUBLE_VECTOR,
        (SexpType::Logical, false) => feedback::LOGICAL_VECTOR,
        (SexpType::List, _) => feedback::LIST,
        _ => feedback::OTHER,
    }
}

// The compiler.

struct Compiler {
    code: Vec<u32>,
    constants: Vec<Sexp>,
    caches: u32,
    loops: Vec<Loop>,
    /// The loops the code being compiled is inside, innermost last, each with the length
    /// [`Compiler::cold`] had when it opened.
    open_loops: Vec<(usize, usize)>,
    registers: u32,
    next_register: u32,
    /// The closure's enclosure, against which the compiler checks that a construct's symbol
    /// names the base primitive.
    enclosure: Sexp,
    /// The symbols the call frame can bind: formals and assignment targets. `None` when the body
    /// reflects on its frame (`assign`, `eval`, ...), so any symbol may appear in it.
    local_symbols: Option<FxHashSet<Sexp>>,
    formals: FxHashSet<Sexp>,
    /// The srcref current where the code being compiled runs: the closure's own, or the
    /// statement's inside a `{` with source references. A `{` makes it current again after its
    /// own statements.
    srcref: Sexp,
    /// The call `x <- f(x, ...)` assigns to `x`, while the compiler compiles that call, so a
    /// call of a closure becomes [`Op::CallRebinding`].
    rebinding: Option<(Sexp, Sexp)>,
    /// The call `x <- call` assigns to `x`, with the symbol's constant and the assignment's
    /// lookup cache slot, while the compiler compiles that call, so a fused pass may compute
    /// the result into the value `x` holds.
    assigning: Option<Sexp>,
    /// The body's calls in tail position that are `f(p, ...)` for a library builtin that
    /// updates its first argument in place, `p` the first parameter, with `p`: each becomes
    /// [`Op::CallTailUpdate`] (see [`tail_updates`]).
    tail_updates: (Vec<Sexp>, Sexp),
    /// While a `match` guard compiles: its arm's bound names, which it reads from registers
    /// instead of the frame.
    bound: Option<Bound>,
    /// Guard fallbacks waiting to be laid out after the body's `Return` or the innermost open
    /// loop's back edge (see [`Compiler::cold_fallback`]).
    cold: Vec<Cold>,
    /// The stored half of the effect-free fact, `NULL` for an argument's code.
    summary: Sexp,
    /// Whether the code is an argument's rather than a closure's body, which an instruction
    /// returning from the activation would return from the wrong frame.
    argument: bool,
    /// The formals in order, when the body's calls keep their variables in slots: the body's
    /// local symbols are known and the formals have no `...`.
    slot_formals: Option<Vec<Sexp>>,
    /// Whether [`Compiler::slot_formals`] is a whole slot table that the code must fit, as for
    /// promise code, rather than the formals the table starts with.
    slots_fixed: bool,
    /// The register an element-wise operator last wrote a fresh result into, with the code
    /// position after it. An operand compiled to exactly that is a temporary only its parent
    /// reads, which may then write its result into the operand's storage (see
    /// [`Op::Arithmetic`]).
    temporary: Option<(u32, u32)>,
    /// While the ordinary code of a fused expression compiles: the positions of the
    /// instructions [`Compiler::fusion_program`] matches against the fusion plan's steps.
    sites: Option<Vec<u32>>,
    /// While a fused expression's ordinary code compiles: the operands [`Compiler::fused_call`]
    /// evaluated ahead of it, each with the register holding its value, in evaluation order.
    /// The ordinary code reads the first entry for an operand in place of evaluating it, and a
    /// later occurrence of the same variable is read afresh.
    hoisted: Vec<(Sexp, u32)>,
    /// The calls of base operators whose name [`Compiler::fused_call`] checked before
    /// evaluating a hoisted operand, each with the function the name was checked to find.
    preguarded: Vec<(Sexp, Sexp)>,
    /// When the code compiles at a time GNU R's JIT would compile it: GNU R's compile-time
    /// frames above the enclosure, the function's locals ([`compiler_locals`]) first and then
    /// each enclosing function's. The code then binds base functions as GNU R's does
    /// ([`Compiler::bound_base`]) and compiles its nested functions at once. `None` keeps a
    /// guard on every base function, which looks the name up as the code runs.
    binding: Option<Vec<Rc<FxHashSet<Sexp>>>>,
    /// For promise code, the function whose frame it runs in, until [`Compiler::force_binding`]
    /// fills `binding` from that function's locals, and `NULL` otherwise. Finding the locals
    /// walks the function's whole body, so it waits for a call that could bind.
    deferred_binding: Sexp,
}

/// A guard fallback compiled out of line: the guards that fail to it, its instruction, and where
/// it resumes in the body.
struct Cold {
    guards: SmallVec<[usize; 2]>,
    words: SmallVec<[u32; 6]>,
    resume: u32,
}

/// The names a `match` arm binds, held in registers `base..base + names.len()` for its guard,
/// with `list` the constant listing them for [`Op::BoundEval`].
struct Bound {
    names: Vec<Sexp>,
    list: u32,
    base: u32,
}

#[derive(Clone, Copy)]
struct Loop {
    start: u32,
    end: u32,
    exit: u32,
    step: u32,
}

/// An element-wise tree [`Compiler::fused_call`] compiles to an [`Op::Fused`]: the nodes and
/// leaves `kernels::fused` takes (a leaf is a variable's symbol, a constant's value or a call
/// operand evaluated ahead), and the instructions the tree's ordinary code is expected to
/// consist of, in order.
struct FusionPlan {
    nodes: Vec<Node>,
    leaves: Vec<Sexp>,
    steps: Vec<FusionStep>,
    reduction: Option<Reduction>,
}

impl FusionPlan {
    fn leaf(&mut self, leaf: Sexp) -> usize {
        self.leaves.push(leaf);
        self.nodes.push(Node::Leaf(self.leaves.len() - 1));
        self.nodes.len() - 1
    }

    /// `call` as a leaf the fused expression evaluates into a register ahead of the pass,
    /// together with the variables read before it. That keeps R's order of evaluation only
    /// while no operator has computed anything yet, and only for a call that cannot modify a
    /// variable in place, because registers do not count as references. The root is never a
    /// leaf, which would fuse the call into itself.
    unsafe fn call_leaf(&mut self, call: Sexp) -> Option<usize> {
        let nothing_computed = !self.steps.is_empty()
            && !self
                .steps
                .iter()
                .any(|step| matches!(step, FusionStep::Result(_)));
        if !nothing_computed || may_modify_in_place(call) {
            return None;
        }
        self.steps.push(FusionStep::Call(self.leaves.len()));
        Some(self.leaf(call))
    }
}

enum FusionStep {
    /// The guard or function lookup that checks the operator `symbol` of `call` names
    /// `expected`: `Head(call, symbol, expected)`.
    Head(Sexp, Sexp, Sexp),
    /// The call operand of leaf `index`, evaluated into a register ahead of the fused pass.
    Call(usize),
    /// The variable read of leaf `index`.
    Variable(usize),
    /// The instruction that computes node `index`, or the reduction when `None`.
    Result(Option<usize>),
}

/// The layout of an [`Op::Fused`] program, an integer vector: a header, then per guard
/// `[symbol, expected, cache slot, probe]` (constants and the operands of the operator's guard
/// or function lookup, `probe` set when the frame itself may bind the name), per leaf
/// `[kind, constant, cache slot, site]` (kind 0 for a constant, 1 for a variable and 2 for a
/// hoisted call operand, `constant` the value of a constant, the symbol of a variable or the
/// register of a call operand), and per node `[tag, a, b, c, site]`. A node's tag is 0 for leaf `a`, 1 for
/// `ARITHS[a]` on nodes `b` and `c`, 2 for `&` (`a` set) or `|` on `b` and `c`, 3 for unary `-`
/// on `a`, 4 for `!` on `a`, and 5 for `MATHS[a]` on `b`. A site is the position of the ordinary
/// instruction whose type feedback the fused pass records.
mod fusion {
    /// How often the pass declined in a row; it stops trying at [`GIVE_UP`].
    pub const DECLINES: usize = 0;
    pub const NODES: usize = 1;
    pub const LEAVES: usize = 2;
    /// -1 for none, [`ASSIGNED`] for none with the result assigned by the `SetVar` the pass
    /// jumps to, else 0 for `sum`, 1 for `any` and 2 for `all`.
    pub const REDUCTION: usize = 3;
    pub const REDUCTION_SITE: usize = 4;
    pub const GUARDS: usize = 5;
    pub const HEADER: usize = 6;
    pub const ASSIGNED: i32 = -2;
    pub const GUARD: usize = 4;
    pub const LEAF: usize = 4;
    pub const NODE: usize = 5;
    pub const GIVE_UP: i32 = 16;
}

/// Compiles a closure body, or reports that it is better left to the AST evaluator. The
/// compiler's vectors are not allocations the R code asked for, so `Rprofmem` does not see them.
unsafe fn compile(body: Sexp, function: Sexp, binds: bool) -> R<Sexp> {
    let binding = if binds {
        Some(vec![Rc::new(compiler_locals(
            function.car(),
            body,
            function.tag(),
        )?)])
    } else {
        None
    };
    compile_with_locals(body, function, local_symbols(body, function.car()), binding)
}

/// [`compile`] with the symbols the frame can bind given, or `None` for code that reads and
/// writes an environment's bindings, such as a loop in the global environment.
unsafe fn compile_with_locals(
    body: Sexp,
    function: Sexp,
    locals: Option<FxHashSet<Sexp>>,
    binding: Option<Vec<Rc<FxHashSet<Sexp>>>>,
) -> R<Sexp> {
    let threshold = crate::heap().swap_report_threshold(usize::MAX);
    let compiled = compile_unreported(body, function, locals, binding);
    crate::heap().swap_report_threshold(threshold);
    compiled
}

unsafe fn compile_unreported(
    body: Sexp,
    function: Sexp,
    locals: Option<FxHashSet<Sexp>>,
    binding: Option<Vec<Rc<FxHashSet<Sexp>>>>,
) -> R<Sexp> {
    let binds = binding.is_some();
    let mut formal_symbols = FxHashSet::default();
    let mut formal = function.car();
    while formal.kind() == SexpType::Pairlist {
        formal_symbols.insert(formal.tag());
        formal = formal.cdr();
    }
    let mut compiler = Compiler {
        local_symbols: locals,
        formals: formal_symbols,
        code: Vec::new(),
        constants: Vec::new(),
        caches: 0,
        loops: Vec::new(),
        open_loops: Vec::new(),
        registers: 0,
        next_register: 0,
        enclosure: function.tag(),
        srcref: values::get_attr(function, symbols().srcref),
        rebinding: None,
        assigning: None,
        bound: None,
        tail_updates: tail_updates(body, function),
        cold: Vec::new(),
        argument: false,
        summary: nil(),
        slot_formals: None,
        slots_fixed: false,
        temporary: None,
        sites: None,
        hoisted: Vec::new(),
        preguarded: Vec::new(),
        binding,
        deferred_binding: nil(),
    };
    if compiler.local_symbols.is_some() {
        let mut formals = Vec::new();
        let mut formal = function.car();
        while formal.kind() == SexpType::Pairlist {
            formals.push(formal.tag());
            formal = formal.cdr();
        }
        if formals.len() <= MAX_SLOT_FORMALS
            && !crate::base_fast_paths::prologue_needs_environment(body)
        {
            compiler.slot_formals = Some(formals);
        }
    }
    let slot = compiler.cache();
    compiler.summary =
        effects::summarize(function.car(), body, compiler.local_symbols.as_ref(), slot)?;
    let code = compiler.assemble(body)?;
    if binds {
        code.set_gp_bits(gp::BINDING_CODE, true);
    }
    Ok(code)
}

/// The symbols a call frame of a closure with `formals` and `body` can bind, or `None` when the
/// body names a function that binds in a frame it is given (so the set cannot be known). A callee
/// could still assign into this frame through `parent.frame()`; like GNU R's compiler, this
/// assumes it does not rebind the syntax and operators the guards check.
pub unsafe fn local_symbols(body: Sexp, formals: Sexp) -> Option<FxHashSet<Sexp>> {
    let mut symbols_found = FxHashSet::default();
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        symbols_found.insert(formal.tag());
        formal = formal.cdr();
    }
    if collect_bindings(body, &mut symbols_found) {
        Some(symbols_found)
    } else {
        None
    }
}

/// A binding's value with a forced promise replaced by its value; an unforced promise stays.
unsafe fn forced_value(value: Sexp) -> Sexp {
    if value.kind() == SexpType::Promise && value.car() != unbound() {
        value.car()
    } else {
        value
    }
}

/// The builtin behind `.Internal(inner)` when the call can run as compiled code: an internal
/// that takes evaluated arguments and never dispatches, called without `...`, empty arguments,
/// or arguments that could modify an earlier one in place, so that evaluating them into
/// registers is what the AST evaluator would do.
unsafe fn compiled_internal(inner: Sexp) -> R<Option<Sexp>> {
    if inner.kind() != SexpType::Language || inner.car().kind() != SexpType::Symbol {
        return Ok(None);
    }
    let Some(function) = crate::builtins::internal_function(inner.car()) else {
        return Ok(None);
    };
    if function.kind() != SexpType::Builtin || eval::builtin_def(function)?.generic != Generic::None
    {
        return Ok(None);
    }
    let args = values::pairlist_items(inner.cdr());
    let simple = args
        .iter()
        .all(|(_, value)| *value != symbols().dots && *value != missing_arg())
        && !args
            .iter()
            .skip(1)
            .any(|(_, value)| may_modify_in_place(*value));
    Ok(simple.then_some(function))
}

/// A constructor call's items, the layout it builds and each item's slot.
type ConstructorPlan<'a> = (&'a [(Sexp, Sexp)], u32, Vec<usize>);

/// The items of a `.ry_record`, `.ry_tuple` or `.ry_tag` call named `name`, the layout it
/// builds and each item's slot, when its syntax fixes them and evaluating the items into
/// registers is what the builtin's own evaluation would do.
unsafe fn constructor<'a>(name: &str, args: &'a [(Sexp, Sexp)]) -> Option<ConstructorPlan<'a>> {
    if args
        .iter()
        .any(|(_, value)| *value == symbols().dots || *value == missing_arg())
        || args
            .iter()
            .skip(1)
            .any(|(_, value)| may_modify_in_place(*value))
    {
        return None;
    }
    let (tag, items) = if name == ".ry_tag" {
        let ((name_tag, tag), items) = args.split_first()?;
        if *name_tag != nil() {
            return None;
        }
        (Some(*tag), items)
    } else {
        (None, args)
    };
    let names: Vec<Sexp> = items.iter().map(|(tag, _)| *tag).collect();
    let (layout, slots) = crate::records::constructor_layout(tag, &names, name == ".ry_record")?;
    Some((items, layout, slots))
}

/// What a tuple, record or tag pattern tests.
enum PatternShape {
    /// `:Name(..)`: the tag, whatever its payload.
    Tag(u32),
    /// A pattern whose syntax fixes the layout, with each item's sub-pattern and slot.
    Layout {
        layout: u32,
        items: Vec<(Sexp, usize)>,
    },
}

/// The shape a `.ry_tuple`, `.ry_record` or `.ry_tag` pattern tests, or `None` when only the
/// `match` special handles it: a `..` that leaves the layout open, or a pattern the special
/// rejects with an error.
unsafe fn pattern_shape(pattern: Sexp) -> Option<PatternShape> {
    let head = pattern.car().car().bytes();
    let items = values::pairlist_items(pattern.cdr());
    let (tag, payload) = match head {
        b".ry_tag" => {
            let ((name_tag, name), payload) = items.split_first()?;
            if *name_tag != nil() {
                return None;
            }
            (Some(*name), payload)
        }
        b".ry_tuple" | b".ry_record" => (None, &items[..]),
        _ => return None,
    };
    let rest = |item: Sexp| item.kind() == SexpType::Symbol && item.car().bytes() == b"..";
    if let (Some(name), [(item_tag, item)]) = (tag, payload)
        && *item_tag == nil()
        && rest(*item)
    {
        return crate::records::tag_id(name).ok().map(PatternShape::Tag);
    }
    if payload.iter().any(|(_, item)| rest(*item)) {
        return None;
    }
    let names: Vec<Sexp> = payload.iter().map(|(tag, _)| *tag).collect();
    let (layout, slots) = crate::records::constructor_layout(tag, &names, head == b".ry_record")?;
    let items = payload.iter().map(|(_, item)| *item).zip(slots).collect();
    Some(PatternShape::Layout { layout, items })
}

/// Whether a `match` guard can compile with the arm's bound `names` read from registers: it must
/// not assign, create a closure, nest a `match`, leave a loop or function, or call a bound name,
/// since each of those would reach the frame or behave differently outside the special's
/// scratch frame. Calls the compiler cannot inline evaluate in a scratch frame anyway.
unsafe fn guard_compiles_in_place(guard: Sexp, names: &[Sexp]) -> bool {
    match guard.kind() {
        SexpType::Symbol => {
            guard != symbols().dots
                && guard != missing_arg()
                && eval::dot_dot_index(guard).is_none()
        }
        SexpType::Language => {
            let head = guard.car();
            if head.kind() != SexpType::Symbol || names.contains(&head) {
                return false;
            }
            let refused = matches!(
                head.car().bytes(),
                b"<-"
                    | b"="
                    | b"<<-"
                    | b"->"
                    | b"->>"
                    | b"assign"
                    | b"function"
                    | b"local"
                    | b".Internal"
                    | b".ry_match"
                    | b"for"
                    | b"while"
                    | b"repeat"
                    | b"break"
                    | b"next"
                    | b"return"
                    | b"on.exit"
                    | b"delayedAssign"
            );
            !refused
                && values::pairlist_items(guard.cdr())
                    .iter()
                    .all(|&(_, argument)| guard_compiles_in_place(argument, names))
        }
        SexpType::Promise | SexpType::Bytecode | SexpType::Dots => false,
        _ => true,
    }
}

/// Whether a `switch` call with these arguments compiles: the selector is a plain first
/// argument, there are alternatives (the special warns when there are none), no alternative is
/// `...` (whose alternatives are only known at run time), and at most one alternative is a
/// default (the special reports a second one as an error).
unsafe fn switch_compiles(args: &[(Sexp, Sexp)]) -> bool {
    let Some(((selector_tag, selector), alternatives)) = args.split_first() else {
        return false;
    };
    let defaults = alternatives
        .iter()
        .filter(|(tag, _)| switch_default(*tag))
        .count();
    *selector_tag == nil()
        && *selector != symbols().dots
        && *selector != missing_arg()
        && !alternatives.is_empty()
        && alternatives
            .iter()
            .all(|(_, value)| *value != symbols().dots)
        && defaults <= 1
}

/// Whether an alternative with this tag is the default: it has no name, or an empty one.
unsafe fn switch_default(tag: Sexp) -> bool {
    tag == nil() || (tag.kind() == SexpType::Symbol && tag.car().bytes().is_empty())
}

/// Where [`switch_table`] keeps the alternatives' names, as a character vector.
const SWITCH_NAMES: usize = 0;
/// Where it keeps, for each name, the arm that name runs (0 for none).
const SWITCH_NAME_ARMS: usize = 1;
/// Where it keeps the default's arm (first; 0 for none, -1 for an empty default, which is an
/// error) and then, for each position, the arm a numeric selector runs (-1 for an empty
/// alternative, which is an error).
const SWITCH_POSITION_ARMS: usize = 2;
/// Where it keeps, for arms 0, 1, 2, ..., the constant pool index of the integer scalar
/// [`Op::SwitchArm`] returns for that arm. The arm tests compare those scalars by identity, and a
/// code image keeps each pool entry as one object but does not keep an object shared between two
/// constants, so the table names the scalars by pool index rather than holding them.
const SWITCH_NUMBERS: usize = 3;
/// Where it keeps the `switch` call, which its errors name.
const SWITCH_CALL: usize = 4;

/// The table [`Op::SwitchArm`] reads for a `switch` with these alternatives, and the arms: the
/// non-empty alternatives in order, numbered from 1. A name runs the first non-empty alternative
/// at or after its own, as the special falls through empty ones.
unsafe fn switch_table(call: Sexp, alternatives: &[(Sexp, Sexp)]) -> R<(Sexp, Vec<Sexp>)> {
    let missing = missing_arg();
    let mut arms = Vec::new();
    let mut position_arms = vec![0];
    for (tag, value) in alternatives {
        if *value == missing {
            position_arms.push(-1);
            if switch_default(*tag) {
                position_arms[0] = -1;
            }
        } else {
            arms.push(*value);
            position_arms.push(arms.len() as i32);
            if switch_default(*tag) {
                position_arms[0] = arms.len() as i32;
            }
        }
    }
    let mut names = Vec::new();
    let mut name_arms = Vec::new();
    for (position, (tag, _)) in alternatives.iter().enumerate() {
        if tag.kind() == SexpType::Symbol {
            names.push(tag.car());
            let arm = position_arms[position + 1..]
                .iter()
                .copied()
                .find(|&arm| arm > 0)
                .unwrap_or(0);
            name_arms.push(arm);
        }
    }
    let heap = crate::heap();
    let name_vector = values::alloc(SexpType::String, names.len())?;
    for (index, name) in names.iter().enumerate() {
        heap.set_element(name_vector, index, *name);
    }
    let name_arms = values::integer_vector(&name_arms)?;
    let position_arms = values::integer_vector(&position_arms)?;
    let numbers = values::integer_vector(&vec![0; arms.len() + 1])?;
    let table = values::list_vector(&[name_vector, name_arms, position_arms, numbers, call])?;
    Ok((table, arms))
}

/// The arm number a `switch` runs for `selector`, from the table [`switch_table`] built, with
/// the special's errors.
unsafe fn switch_arm(selector: Sexp, table: Sexp) -> R<usize> {
    let position_arms = values::ints(table.element(SWITCH_POSITION_ARMS));
    let arm = match eval::switch_selector(selector, table.element(SWITCH_CALL))? {
        SwitchSelector::Name(text) => {
            let names = table.element(SWITCH_NAMES);
            let count = length(names);
            // Names and selectors are interned, so identity finds the name in the common case,
            // and only a missing selector or one in another encoding needs its text compared.
            let matched = (0..count)
                .find(|&index| names.element(index) == text)
                .or_else(|| {
                    (0..count).find(|&index| eval::switch_name_matches(names.element(index), text))
                });
            match matched {
                Some(index) => values::ints(table.element(SWITCH_NAME_ARMS))[index],
                None if position_arms[0] < 0 => return Err(error(eval::SWITCH_EMPTY_DEFAULT)),
                None => position_arms[0],
            }
        }
        SwitchSelector::Position(index) => match position_arms.get(index.max(0) as usize) {
            Some(&arm) if index >= 1 => {
                if arm < 0 {
                    return Err(error("empty alternative in numeric switch"));
                }
                arm
            }
            _ => 0,
        },
    };
    Ok(arm as usize)
}

/// Whether bytecode can run this `.ry_match` call's arms: every arm is `pattern => value` or
/// `pattern => guard, value`, and every pattern is one [`Compiler::pattern`] compiles. Anything
/// else (an open record pattern, a pattern the special rejects) runs the whole `match` in the
/// special.
unsafe fn supported_match(args: &[(Sexp, Sexp)]) -> R<bool> {
    let Some(((_, scrutinee), arms)) = args.split_first() else {
        return Ok(false);
    };
    if args.iter().any(|(tag, _)| *tag != nil())
        || *scrutinee == symbols().dots
        || *scrutinee == missing_arg()
    {
        return Ok(false);
    }
    let arrow = values::install("=>")?;
    let plain = |part: Sexp| part != symbols().dots && part != missing_arg();
    Ok(arms.iter().all(|(_, arm)| {
        if arm.kind() != SexpType::Language || arm.car() != arrow {
            return false;
        }
        match values::pairlist_items(arm.cdr()).as_slice() {
            [(pattern_tag, pattern), (body_tag, body)] => {
                *pattern_tag == nil()
                    && *body_tag == nil()
                    && plain(*body)
                    && supported_pattern(*pattern)
            }
            [(pattern_tag, pattern), (guard_tag, guard), (body_tag, body)] => {
                *pattern_tag == nil()
                    && *guard_tag == nil()
                    && *body_tag == nil()
                    && plain(*guard)
                    && plain(*body)
                    && supported_pattern(*pattern)
            }
            _ => false,
        }
    }))
}

unsafe fn supported_pattern(pattern: Sexp) -> bool {
    match pattern.kind() {
        SexpType::Symbol => {
            !matches!(pattern.car().bytes(), b".." | b"T" | b"F")
                && pattern != symbols().dots
                && pattern != missing_arg()
                && eval::dot_dot_index(pattern).is_none()
        }
        SexpType::Language => {
            if pattern.car().kind() != SexpType::Symbol {
                return false;
            }
            let items = values::pairlist_items(pattern.cdr());
            if items.iter().any(|(tag, _)| *tag != nil())
                && !matches!(pattern.car().car().bytes(), b".ry_record" | b".ry_tag")
            {
                return false;
            }
            match pattern.car().car().bytes() {
                b"(" => matches!(items.as_slice(), [(_, inner)] if supported_pattern(*inner)),
                // The special keeps only the bindings of the alternative that matched, which
                // the compiled tests do not track, so an alternative may not bind a name.
                b"|" => matches!(items.as_slice(), [(_, left), (_, right)]
                    if supported_pattern(*left)
                        && supported_pattern(*right)
                        && pattern_names(*left).is_empty()
                        && pattern_names(*right).is_empty()),
                _ => match pattern_shape(pattern) {
                    Some(PatternShape::Tag(_)) => true,
                    Some(PatternShape::Layout { items, .. }) => {
                        items.iter().all(|(item, _)| supported_pattern(*item))
                    }
                    None => false,
                },
            }
        }
        SexpType::Nil
        | SexpType::Logical
        | SexpType::Integer
        | SexpType::Real
        | SexpType::Complex
        | SexpType::String => true,
        _ => false,
    }
}

/// The names a pattern may bind: every symbol in it other than `_` and `..`, never a call's
/// head.
unsafe fn pattern_names(pattern: Sexp) -> Vec<Sexp> {
    let mut names = Vec::new();
    collect_pattern_names(pattern, &mut names);
    names
}

unsafe fn collect_pattern_names(pattern: Sexp, names: &mut Vec<Sexp>) {
    match pattern.kind() {
        SexpType::Symbol => {
            if !matches!(pattern.car().bytes(), b"_" | b"..") {
                names.push(pattern);
            }
        }
        SexpType::Language => {
            for (_, item) in values::pairlist_items(pattern.cdr()) {
                collect_pattern_names(item, names);
            }
        }
        _ => {}
    }
}

/// Whether `index` is a constant `[[` index a record or tuple item read can cache: one
/// positive whole number or one string.
unsafe fn constant_item(index: Sexp) -> bool {
    if length(index) != 1 || values::has_attributes(index) {
        return false;
    }
    match index.kind() {
        SexpType::Integer | SexpType::Real => {
            let position = values::as_real_scalar(index);
            position >= 1.0 && position.fract() == 0.0 && position <= f64::from(i32::MAX)
        }
        SexpType::String => index.element(0) != values::na_string(),
        _ => false,
    }
}

/// Whether evaluating `expression` may modify a variable of the frame in place: it contains a
/// replacement assignment such as `x[1] <- 9`, or code that evaluates other code in the frame.
unsafe fn may_modify_in_place(expression: Sexp) -> bool {
    if expression.kind() != SexpType::Language {
        return false;
    }
    let head = expression.car();
    if head.kind() == SexpType::Symbol {
        let name = symbol_name(head);
        if matches!(&*name, "<-" | "=" | "<<-")
            && expression.cdr().car().kind() == SexpType::Language
        {
            return true;
        }
        if rebinds_first_argument(expression) || assigns_fusible(expression) {
            return true;
        }
        if matches!(
            &*name,
            "eval" | "evalq" | "local" | "with" | "within" | "source" | "sys.function" | "do.call"
        ) {
            return true;
        }
    }
    let mut cell = expression;
    while cell.kind() == SexpType::Language || cell.kind() == SexpType::Pairlist {
        if may_modify_in_place(cell.car()) {
            return true;
        }
        cell = cell.cdr();
    }
    false
}

/// Whether `expression` is `x <- call` for an element-wise operator or math function, whose
/// fused pass may compute its result into the value `x` holds (`kernels::fused`), so an
/// earlier argument of an enclosing call must not keep `x` in an uncounted register.
unsafe fn assigns_fusible(expression: Sexp) -> bool {
    let head = expression.car();
    if head.kind() != SexpType::Symbol || !matches!(&*symbol_name(head), "<-" | "=") {
        return false;
    }
    let value = expression.cdr().cdr().car();
    if expression.cdr().car().kind() != SexpType::Symbol
        || value.kind() != SexpType::Language
        || value.car().kind() != SexpType::Symbol
    {
        return false;
    }
    let operator = symbol_name(value.car());
    ARITHS.iter().any(|(_, name)| *name == &*operator)
        || math_index(&operator).is_some()
        || matches!(&*operator, "&" | "|" | "!")
}

/// Whether `expression` is `x <- f(x, ...)`, whose call may update the value of `x` in place
/// (`eval::rebind_call`), so an earlier argument of an enclosing call must not keep `x` in an
/// uncounted register.
unsafe fn rebinds_first_argument(expression: Sexp) -> bool {
    let head = expression.car();
    if head.kind() != SexpType::Symbol || !matches!(&*symbol_name(head), "<-" | "=") {
        return false;
    }
    let target = expression.cdr().car();
    let value = expression.cdr().cdr().car();
    target.kind() == SexpType::Symbol
        && value.kind() == SexpType::Language
        && value.car().kind() == SexpType::Symbol
        && value.cdr().kind() == SexpType::Pairlist
        && value.cdr().car() == target
        && value.cdr().tag() == nil()
}

/// The calls in tail position of `body` that are `f(p, ...)` for a standard library builtin that
/// updates its first argument in place, with `p` the first parameter of `function`, and `p`. A
/// call is in tail position when it is the body, the last statement of a `{` in tail position, a
/// branch of an `if` in tail position, or the argument of a `return` in a statement of the body
/// outside any function literal or `local`. A move into the frame leaves the parameter holding
/// the only reference to the caller's value, and nothing runs in the frame after such a call, so
/// updating the parameter's value in place is invisible while the move lasts.
unsafe fn tail_updates(body: Sexp, function: Sexp) -> (Vec<Sexp>, Sexp) {
    let formals = function.car();
    let mut calls = Vec::new();
    if formals.kind() != SexpType::Pairlist {
        return (calls, nil());
    }
    let parameter = formals.tag();
    collect_tail_updates(body, parameter, true, &mut calls);
    // The compiler finds a tail call by identity, and a body built with `bquote` or `body<-`
    // can share one call object between a tail and a non-tail position, so a call that occurs
    // more than once gets no update.
    if !calls.is_empty() {
        let mut occurrences = vec![0usize; calls.len()];
        count_occurrences(body, &calls, &mut occurrences);
        let mut position = 0;
        calls.retain(|_| {
            position += 1;
            occurrences[position - 1] == 1
        });
    }
    (calls, parameter)
}

/// Adds, for each of `calls`, how often the object occurs within `expression`.
unsafe fn count_occurrences(expression: Sexp, calls: &[Sexp], occurrences: &mut [usize]) {
    if let Some(position) = calls.iter().position(|&call| call == expression) {
        occurrences[position] += 1;
    }
    let mut cell = expression;
    while cell.kind() == SexpType::Language || cell.kind() == SexpType::Pairlist {
        count_occurrences(cell.car(), calls, occurrences);
        cell = cell.cdr();
    }
}

/// Adds the tail updates of `expression` to `calls`: the expression itself when `tail`, and the
/// arguments of `return` in the statements it runs.
unsafe fn collect_tail_updates(
    expression: Sexp,
    parameter: Sexp,
    tail: bool,
    calls: &mut Vec<Sexp>,
) {
    if expression.kind() != SexpType::Language || expression.car().kind() != SexpType::Symbol {
        return;
    }
    let mut arguments = Vec::new();
    let mut argument = expression.cdr();
    while argument.kind() == SexpType::Pairlist {
        arguments.push(argument.car());
        argument = argument.cdr();
    }
    match (&*symbol_name(expression.car()), arguments.as_slice()) {
        ("{", statements) => {
            for (index, &statement) in statements.iter().enumerate() {
                let last = index + 1 == statements.len();
                collect_tail_updates(statement, parameter, tail && last, calls);
            }
        }
        ("if", [condition, branches @ ..]) if branches.len() <= 2 => {
            collect_tail_updates(*condition, parameter, false, calls);
            for &branch in branches {
                collect_tail_updates(branch, parameter, tail, calls);
            }
        }
        ("for", [_, _, statement]) | ("while", [_, statement]) | ("repeat", [statement]) => {
            collect_tail_updates(*statement, parameter, false, calls);
        }
        ("return", [value]) => collect_tail_updates(*value, parameter, true, calls),
        (name, [first, ..])
            if tail
                && *first == parameter
                && expression.cdr().tag() == nil()
                && crate::standard_library::reuses_first_argument(name) =>
        {
            calls.push(expression);
        }
        _ => {}
    }
}

/// Whether `expression` is `x <- f(x, ...)` for a standard library builtin that updates its
/// first argument in place. The AST evaluator decides at run time whether the binding owns the
/// value (`eval::rebind`), so the compiler leaves the whole assignment to it.
unsafe fn reuses_first_argument(expression: Sexp) -> bool {
    rebinds_first_argument(expression)
        && crate::standard_library::reuses_first_argument(&symbol_name(
            expression.cdr().cdr().car().car(),
        ))
}

/// Adds the symbols `expression` assigns locally; `false` when it may bind a name the code does
/// not show in its own frame, or hands that frame to code that may. A call that names another
/// frame or a new one explicitly (`eval(e, envir)`, `assign(x, v, envir = e)`, `do.call(f, args)`
/// with `f` a function) binds nothing here; `environment()` itself is what would hand this frame
/// over, and it is refused wherever it appears. The call inside `.Internal(...)` names an
/// internal, which binds only in the environments its arguments give it.
unsafe fn collect_bindings(expression: Sexp, found: &mut FxHashSet<Sexp>) -> bool {
    if expression.kind() == SexpType::Symbol {
        // A method's slot frame keeps `.Generic` and its kin beside the frame and binds them
        // only when it makes its environment, so a body that reads one by name needs the
        // environment from the start.
        let symbols = symbols();
        return ![
            symbols.dot_generic,
            symbols.dot_class,
            symbols.dot_method,
            symbols.dot_group,
            symbols.dot_generic_call_env,
            symbols.dot_generic_def_env,
        ]
        .contains(&expression);
    }
    if expression.kind() != SexpType::Language {
        return true;
    }
    let head = expression.car();
    if head.kind() == SexpType::Symbol {
        let name = symbol_name(head);
        if binds_in_caller(&name, expression.cdr()) {
            return false;
        }
        // A `match` arm binds its pattern's names in the frame.
        if &*name == ".ry_match" {
            for (_, arm) in values::pairlist_items(expression.cdr()).iter().skip(1) {
                if arm.kind() == SexpType::Language {
                    found.extend(pattern_names(arm.cdr().car()));
                }
            }
        }
        if matches!(&*name, "<-" | "=" | "for") {
            let mut target = expression.cdr().car();
            while target.kind() == SexpType::Language {
                target = target.cdr().car();
            }
            if target.kind() == SexpType::Symbol {
                found.insert(target);
            } else if target.kind() == SexpType::String && length(target) == 1 {
                return false;
            }
        }
        if &*name == "function" {
            return true;
        }
        if &*name == ".Internal" {
            let inner = expression.cdr().car();
            if inner.kind() == SexpType::Language {
                return collect_arguments(inner.cdr(), found);
            }
        }
    }
    collect_arguments(expression, found)
}

unsafe fn collect_arguments(mut cell: Sexp, found: &mut FxHashSet<Sexp>) -> bool {
    while cell.kind() == SexpType::Pairlist || cell.kind() == SexpType::Language {
        if !collect_bindings(cell.car(), found) {
            return false;
        }
        cell = cell.cdr();
    }
    true
}

/// Whether the call of `name` with `arguments` may bind in the frame that makes it, or return
/// that frame: by default `eval`, `assign` and their kin act on their caller's frame.
unsafe fn binds_in_caller(name: &str, arguments: Sexp) -> bool {
    let given = |position: usize, tag: &str| {
        let mut cell = arguments;
        let mut index = 0;
        while cell.kind() == SexpType::Pairlist {
            let cell_tag = cell.tag();
            if cell_tag != nil() {
                if &*symbol_name(cell_tag) == tag {
                    return true;
                }
            } else {
                if index == position {
                    return true;
                }
                index += 1;
            }
            cell = cell.cdr();
        }
        false
    };
    match name {
        "eval" | "evalq" => !given(1, "envir"),
        "assign" => !(given(2, "pos") || given(3, "envir")),
        "delayedAssign" => !given(3, "assign.env"),
        "rm" | "remove" => !(given(usize::MAX, "envir") || given(usize::MAX, "pos")),
        "environment" => {
            let argument = arguments.car();
            arguments == nil() || argument == nil()
        }
        "sys.frame" => {
            let argument = arguments.car();
            arguments == nil()
                || (argument.kind() == SexpType::Language
                    && argument.car().kind() == SexpType::Symbol
                    && &*symbol_name(argument.car()) == "sys.nframe")
        }
        "do.call" => {
            let what = arguments.car();
            arguments == nil()
                || match what.kind() {
                    SexpType::String if length(what) == 1 => {
                        refused_callee(&values::char_str(values::string_elt(what, 0)))
                    }
                    SexpType::Symbol => refused_callee(&symbol_name(what)),
                    _ => false,
                }
        }
        "makeActiveBinding" | "list2env" | "local" | "with" | "within" | "lockBinding"
        | "sys.function" | "on.exit" => false,
        _ => refused_callee(name),
    }
}

/// The functions that bind in or hand over their caller's frame however they are called.
fn refused_callee(name: &str) -> bool {
    matches!(
        name,
        "eval"
            | "evalq"
            | "assign"
            | "delayedAssign"
            | "rm"
            | "remove"
            | "environment"
            | "sys.frame"
            | "sys.frames"
            | "attach"
            | "source"
            | "do.call"
            | "sys.function"
    )
}

impl Compiler {
    /// Compiles `source` as the whole code of a new code object, and builds that object.
    unsafe fn assemble(&mut self, source: Sexp) -> R<Sexp> {
        let result = self.allocate();
        self.expression(source, result)?;
        self.emit(Op::Return, &[result]);
        for cold in std::mem::take(&mut self.cold) {
            for guard in cold.guards {
                self.patch(guard);
            }
            self.code.extend_from_slice(&cold.words);
            self.emit(Op::Resume, &[cold.resume]);
        }
        let compiler = &*self;
        let heap = crate::heap();
        let words = values::alloc(SexpType::Integer, compiler.code.len())?;
        for (index, word) in compiler.code.iter().enumerate() {
            values::ints_mut(words)[index] = *word as i32;
        }
        let constants = values::alloc(SexpType::List, FIRST_CONSTANT + compiler.constants.len())?;
        heap.set_element(constants, SOURCE, source);
        let epochs = values::alloc(SexpType::Real, compiler.caches as usize)?;
        // No stamp is all ones, so every cache starts empty.
        values::reals_mut(epochs).fill(f64::from_bits(u64::MAX));
        heap.set_element(constants, EPOCHS, epochs);
        let hints = values::alloc(SexpType::Integer, compiler.caches as usize)?;
        values::ints_mut(hints).fill(0);
        heap.set_element(constants, HINTS, hints);
        let feedback = values::alloc(SexpType::Integer, compiler.code.len())?;
        values::ints_mut(feedback).fill(0);
        heap.set_element(constants, FEEDBACK, feedback);
        let profile = values::alloc(SexpType::Integer, profile::LENGTH)?;
        values::ints_mut(profile).fill(0);
        values::ints_mut(profile)[profile::RECORDING] =
            i32::from(!compiler.loops.is_empty() && interp().bytecode.tier.is_some());
        values::ints_mut(profile)[profile::NEXT] = i32::MIN;
        heap.set_element(constants, PROFILE, profile);
        heap.set_element(constants, TIER1, nil());
        heap.set_element(constants, SUMMARY, compiler.summary);
        heap.set_element(constants, SLOTS, compiler.slot_table()?);
        let loops = values::alloc(SexpType::Integer, compiler.loops.len() * 4)?;
        for (index, entry) in compiler.loops.iter().enumerate() {
            let slots = &mut values::ints_mut(loops)[index * 4..index * 4 + 4];
            slots.copy_from_slice(&[
                entry.start as i32,
                entry.end as i32,
                entry.exit as i32,
                entry.step as i32,
            ]);
        }
        heap.set_element(constants, LOOPS, loops);
        let registers = values::scalar_integer(compiler.registers as i32)?;
        heap.set_element(constants, REGISTERS, registers);
        for (index, constant) in compiler.constants.iter().enumerate() {
            heap.set_element(constants, FIRST_CONSTANT + index, *constant);
        }
        index_slots(words, constants)?;
        let caches = values::alloc(SexpType::List, compiler.caches as usize * 2)?;
        heap.mark_weak_pairs(caches);
        let code = values::triple(SexpType::Bytecode, words, constants, caches)?;
        Ok(code)
    }

    /// The [`SLOTS`] constant: the formals take the first slots, and every other local symbol
    /// that a `GetVar`, `SetVar`, `GetFun` or `ForStep` names takes the next free one in code
    /// order, so the table depends on the code alone.
    unsafe fn slot_table(&self) -> R<Sexp> {
        let (Some(formals), Some(locals)) = (&self.slot_formals, &self.local_symbols) else {
            return Ok(nil());
        };
        let mut names = formals.clone();
        let mut table = vec![-1i32; self.caches as usize];
        let mut pc = 0;
        while pc < self.code.len() {
            let op =
                op_of(self.code[pc]).ok_or_else(|| error("compiled an invalid instruction"))?;
            let symbol_and_cache = match op {
                Op::GetVar | Op::GetFun => Some((self.code[pc + 2], Some(self.code[pc + 3]))),
                Op::SetVar => Some((self.code[pc + 1], Some(self.code[pc + 3]))),
                // A loop variable the body never reads still needs a slot, or the first step
                // would give the frame an environment.
                Op::ForStep => Some((self.code[pc + 3], None)),
                _ => None,
            };
            if let Some((constant, cache)) = symbol_and_cache {
                let symbol = self.constants[constant as usize - FIRST_CONSTANT];
                if locals.contains(&symbol) {
                    let slot = match names.iter().position(|&name| name == symbol) {
                        Some(slot) => slot,
                        // The frame keeps this local in no slot, so the code needs its
                        // environment.
                        None if self.slots_fixed => return Ok(nil()),
                        None => {
                            names.push(symbol);
                            names.len() - 1
                        }
                    };
                    if let Some(cache) = cache {
                        table[cache as usize] = slot as i32;
                    }
                }
            }
            pc += 1 + operand_count(op);
        }
        let heap = crate::heap();
        let map = values::alloc(SexpType::Integer, table.len())?;
        heap.root(map);
        values::ints_mut(map).copy_from_slice(&table);
        let symbols = values::alloc(SexpType::List, names.len())?;
        heap.root(symbols);
        for (index, name) in names.iter().enumerate() {
            heap.set_element(symbols, index, *name);
        }
        // The third element is filled by `slot_defaults` on the first call that needs it, and
        // the fourth by `index_slots` once the constants are in place.
        let slots = values::alloc(SexpType::List, 4)?;
        heap.set_element(slots, 0, map);
        heap.set_element(slots, 1, symbols);
        Ok(slots)
    }

    /// The constant for a call that may call a closure, followed by the arguments its promises
    /// are made from: the unbound marker when an argument is worth compiling, which
    /// [`Op::CallGeneric`] replaces with the compiled arguments the first time it calls a
    /// closure here, or `NULL` when none is. A third constant is `TRUE` when a call of an
    /// effect-free closure may bind symbol arguments to their values (see [`eager_site`]). The
    /// site's direct-call plan lives outside the code ([`CallPlans`]).
    unsafe fn call_site(&mut self, call: Sexp) -> R<u32> {
        let index = self.constant(call);
        let arguments = values::pairlist_items(call.cdr());
        let worth_compiling = arguments
            .iter()
            .any(|(_, value)| compiles_as_promise(*value));
        self.constant(if worth_compiling { unbound() } else { nil() });
        let eager = arguments
            .iter()
            .any(|(_, value)| value.kind() == SexpType::Symbol)
            && arguments
                .iter()
                .all(|(_, value)| self.cannot_assign(*value, 0));
        let flag = if eager {
            values::scalar_logical(1)?
        } else {
            nil()
        };
        self.constant(flag);
        Ok(index)
    }

    /// Whether evaluating the argument `expression` in this frame can assign no variable, so
    /// that reading another argument's variable when the call starts gives what reading it when
    /// the callee forces it would: a variable, a constant, or base builtins that touch nothing
    /// but their arguments, called on those, with no name this frame could rebind.
    unsafe fn cannot_assign(&self, expression: Sexp, depth: u32) -> bool {
        match expression.kind() {
            SexpType::Symbol => {
                expression != symbols().dots
                    && expression != missing_arg()
                    && eval::dot_dot_index(expression).is_none()
            }
            SexpType::Language if depth < 8 && expression.car().kind() == SexpType::Symbol => {
                let head = expression.car();
                !matches!(&*symbol_name(head), "<-" | "=" | "[<-" | "[[<-")
                    && effects::EFFECT_FREE_BUILTINS.contains(&&*symbol_name(head))
                    && !self.frame_may_bind(head)
                    && values::pairlist_items(expression.cdr())
                        .iter()
                        .all(|(_, argument)| self.cannot_assign(*argument, depth + 1))
            }
            SexpType::Language | SexpType::Promise | SexpType::Bytecode => false,
            _ => true,
        }
    }

    unsafe fn frame_may_bind(&self, symbol: Sexp) -> bool {
        self.local_symbols
            .as_ref()
            .is_none_or(|found| found.contains(&symbol))
    }

    fn allocate(&mut self) -> u32 {
        let register = self.next_register;
        self.next_register += 1;
        self.registers = self.registers.max(self.next_register);
        register
    }

    fn release_to(&mut self, mark: u32) {
        self.next_register = mark;
    }

    fn emit(&mut self, op: Op, operands: &[u32]) {
        if let Some(sites) = &mut self.sites
            && matches!(
                op,
                Op::Guard | Op::GetFun | Op::GetVar | Op::Arithmetic | Op::CallBuiltin
            )
        {
            sites.push(self.code.len() as u32);
        }
        let word = if op == Op::CallBuiltin
            && let [_, _, call, _, count] = *operands
            && let Some(&call) = (call as usize)
                .checked_sub(FIRST_CONSTANT)
                .and_then(|index| self.constants.get(index))
            // SAFETY: the compiler roots every constant it holds, and a call is a pairlist.
            && unsafe { positional_call(call, count as usize) }
        {
            POSITIONAL_CALL_BUILTIN_WORD
        } else {
            op_word(op)
        };
        self.code.push(word);
        self.code.extend_from_slice(operands);
    }

    /// Emits a jump-like instruction whose last operand is a target patched later, and returns
    /// that operand's position.
    fn emit_patchable(&mut self, op: Op, operands: &[u32]) -> usize {
        self.emit(op, operands);
        self.code.len() - 1
    }

    fn patch(&mut self, position: usize) {
        self.code[position] = self.code.len() as u32;
    }

    fn here(&self) -> u32 {
        self.code.len() as u32
    }

    fn constant(&mut self, value: Sexp) -> u32 {
        crate::heap().root(value);
        self.constants.push(value);
        (FIRST_CONSTANT + self.constants.len() - 1) as u32
    }

    fn cache(&mut self) -> u32 {
        self.caches += 1;
        self.caches - 1
    }

    unsafe fn expression(&mut self, expression: Sexp, dst: u32) -> R<()> {
        match expression.kind() {
            SexpType::Symbol | SexpType::Language
                if let Some(index) = self
                    .hoisted
                    .iter()
                    .position(|&(operand, _)| operand == expression) =>
            {
                let (_, register) = self.hoisted.remove(index);
                self.emit(Op::Move, &[dst, register]);
                Ok(())
            }
            SexpType::Symbol => self.symbol(expression, dst),
            SexpType::Language => self.call(expression, dst),
            SexpType::Promise | SexpType::Bytecode | SexpType::Dots => {
                self.fallback(expression, dst)
            }
            _ => {
                let constant = self.constant(expression);
                self.emit(Op::Constant, &[dst, constant]);
                Ok(())
            }
        }
    }

    unsafe fn symbol(&mut self, symbol: Sexp, dst: u32) -> R<()> {
        if symbol == symbols().dots
            || eval::dot_dot_index(symbol).is_some()
            || symbol == missing_arg()
        {
            return self.fallback(symbol, dst);
        }
        // A pattern that binds a name twice leaves the last value bound, as the special does.
        if let Some(bound) = &self.bound
            && let Some(index) = bound.names.iter().rposition(|&name| name == symbol)
        {
            let register = bound.base + index as u32;
            self.emit(Op::Move, &[dst, register]);
            return Ok(());
        }
        let constant = self.constant(symbol);
        let cache = self.cache();
        self.emit(Op::GetVar, &[dst, constant, cache]);
        Ok(())
    }

    unsafe fn fallback(&mut self, expression: Sexp, dst: u32) -> R<()> {
        let constant = self.constant(expression);
        self.evaluate(dst, constant);
        Ok(())
    }

    /// A guard that `head` still finds `expected`, the base function the call is compiled for,
    /// with the fallback for when it does not; or no guard when GNU R's compiler binds the name
    /// to that function here (see [`Compiler::bound_base`]), so the call is compiled for it
    /// whatever the name finds when the code runs.
    unsafe fn guard(&mut self, call: Sexp, head: Sexp, expected: Sexp) -> R<Option<usize>> {
        if self.bound_base(call, head)? == Some(expected) {
            if let Some(sites) = &mut self.sites {
                sites.push(UNGUARDED);
            }
            return Ok(None);
        }
        let symbol = self.constant(head);
        let expected = self.constant(expected);
        let cache = self.cache();
        let probe = self.frame_may_bind(head) as u32;
        Ok(Some(self.emit_patchable(
            Op::Guard,
            &[symbol, expected, cache, probe, 0],
        )))
    }

    /// The base function GNU R's compiler binds `head` to in `call`, or `None` when the code
    /// looks the name up as it runs: always for code compiled at a time GNU R would not have
    /// compiled it, and otherwise when the name is one of the function's locals or of an
    /// enclosing function's, or [`base_binding_at_compile`] or [`handler_binds`] says so.
    unsafe fn bound_base(&mut self, call: Sexp, head: Sexp) -> R<Option<Sexp>> {
        if head.kind() != SexpType::Symbol
            || (self.binding.is_none() && self.deferred_binding == nil())
        {
            return Ok(None);
        }
        let Some(value) = base_binding_at_compile(head, self.enclosure) else {
            return Ok(None);
        };
        if !handler_binds(&symbol_name(head), call, self.enclosure) {
            return Ok(None);
        }
        self.force_binding()?;
        let Some(frames) = &self.binding else {
            return Ok(None);
        };
        Ok((!frames.iter().any(|frame| frame.contains(&head))).then_some(value))
    }

    /// Fills `binding` for promise code from its function's locals, the first time a call
    /// could bind (see `deferred_binding`).
    unsafe fn force_binding(&mut self) -> R<()> {
        if self.deferred_binding != nil() {
            let function = self.deferred_binding;
            self.deferred_binding = nil();
            self.binding = promise_binding(function)?;
        }
        Ok(())
    }

    /// Compiles what runs when one of `guards` fails: the general evaluation of `call` into
    /// `dst`, after which execution continues here. The fallback goes out of line, so the inline
    /// path runs straight on instead of jumping over it: after the body's `Return`, or inside a
    /// loop after the loop's back edge (see [`Compiler::back_edge`]).
    unsafe fn cold_fallback(&mut self, guards: &[usize], call: Sexp, dst: u32) -> R<()> {
        // Code that binds its base functions has no guard to fail, so nothing reaches a fallback.
        if guards.is_empty() {
            return Ok(());
        }
        let resume = self.here();
        self.fallback(call, dst)?;
        let words = self.code.drain(resume as usize..).collect();
        self.cold.push(Cold {
            guards: guards.iter().copied().collect(),
            words,
            resume,
        });
        Ok(())
    }

    /// Evaluates the expression in constant `constant` in the frame, or with a guard's bound
    /// names in scope while one compiles.
    fn evaluate(&mut self, dst: u32, constant: u32) {
        match &self.bound {
            Some(bound) => {
                let (list, base, count) = (bound.list, bound.base, bound.names.len() as u32);
                self.emit(Op::BoundEval, &[dst, constant, list, base, count])
            }
            None => self.emit(Op::Ast, &[dst, constant]),
        };
    }

    unsafe fn call(&mut self, call: Sexp, dst: u32) -> R<()> {
        if self.sites.is_none()
            && self.bound.is_none()
            && let Some(plan) = self.fusion_plan(call)?
        {
            return self.fused_call(call, dst, &plan);
        }
        self.unfused_call(call, dst)
    }

    /// An element-wise expression of at least two operators, such as `exp(a * b + c)` or
    /// `sum(x > 0)`, as an [`Op::Fused`] in front of its ordinary code: the fused pass computes
    /// it without intermediate vectors and jumps over the ordinary code, which runs whenever the
    /// pass declines. The ordinary code is compiled exactly as it would be without fusion, and
    /// the fused pass records the type feedback its instructions would have recorded, so the
    /// optimizing tier sees the same code and the same types either way.
    ///
    /// A call operand the pass cannot compute, such as `mean(x)` in `x - mean(x) > 0`, is
    /// evaluated into a register first, and both the pass and the ordinary code read it there,
    /// so it runs exactly once. Before it runs, guards check that the operators around it still
    /// name their base functions, since a redefined operator might not evaluate it at all; when
    /// one does not, the whole call goes to the evaluator instead.
    unsafe fn fused_call(&mut self, call: Sexp, dst: u32, plan: &FusionPlan) -> R<()> {
        let assigned = self.assigning == Some(call);
        let ahead_end = plan
            .steps
            .iter()
            .rposition(|step| matches!(step, FusionStep::Call(_)))
            .map_or(0, |last| last + 1);
        let mark = self.next_register;
        let (hoisted, preguarded) = (self.hoisted.len(), self.preguarded.len());
        let mut guards = Vec::new();
        let mut preguard_sites = Vec::new();
        for step in &plan.steps[..ahead_end] {
            if let FusionStep::Head(operation, symbol, expected) = *step {
                let site = self.here();
                let guard = self.guard(operation, symbol, expected)?;
                preguard_sites.push(if guard.is_some() { site } else { UNGUARDED });
                guards.extend(guard);
                self.preguarded.push((operation, expected));
            }
        }
        let mut ahead = Vec::new();
        for step in &plan.steps[..ahead_end] {
            if let FusionStep::Variable(leaf) | FusionStep::Call(leaf) = *step {
                let register = self.allocate();
                self.expression(plan.leaves[leaf], register)?;
                ahead.push((leaf, register));
            }
        }
        self.hoisted.extend(
            ahead
                .iter()
                .map(|&(leaf, register)| (plan.leaves[leaf], register)),
        );
        let program_position = self.code.len() + 2;
        let target = self.emit_patchable(Op::Fused, &[dst, 0, 0]);
        self.sites = Some(Vec::new());
        let compiled = self.unfused_call(call, dst);
        let ordinary = self.sites.take().unwrap_or_default();
        self.hoisted.truncate(hoisted);
        self.preguarded.truncate(preguarded);
        compiled?;
        self.patch(target);
        // The steps' instructions: the guards above for the operators checked ahead, nothing
        // for the operands evaluated ahead, and the ordinary code's own for the rest.
        let mut preguard_sites = preguard_sites.into_iter();
        let mut ordinary = ordinary.into_iter();
        let mut sites = Vec::with_capacity(plan.steps.len());
        for (index, step) in plan.steps.iter().enumerate() {
            let site = match step {
                FusionStep::Head(..) if index < ahead_end => preguard_sites.next(),
                FusionStep::Variable(_) | FusionStep::Call(_) if index < ahead_end => Some(0),
                _ => ordinary.next(),
            };
            sites.extend(site);
        }
        if ordinary.next().is_some() {
            sites.clear();
        }
        let program = match self.fusion_program(plan, &sites, &ahead, assigned) {
            Some(words) => values::integer_vector(&words)?,
            None => {
                self.code[program_position - 2] = op_word(Op::Unfused);
                nil()
            }
        };
        self.code[program_position] = self.constant(program);
        if !guards.is_empty() {
            let temporary = self.temporary == Some((dst, self.here()));
            self.cold_fallback(&guards, call, dst)?;
            if temporary {
                self.temporary = Some((dst, self.here()));
            }
        }
        self.release_to(mark);
        Ok(())
    }

    /// The fusion plan for `call`, if it is an element-wise tree worth fusing.
    unsafe fn fusion_plan(&mut self, call: Sexp) -> R<Option<FusionPlan>> {
        let head = call.car();
        if head.kind() != SexpType::Symbol {
            return Ok(None);
        }
        // A tree of two operators has one as an operand of the other, so a call without a call
        // among its arguments, which is most calls, is ruled out before any name is read.
        let mut arguments = call.cdr();
        let mut nested = false;
        while arguments.kind() == SexpType::Pairlist {
            nested |= arguments.car().kind() == SexpType::Language;
            arguments = arguments.cdr();
        }
        if !nested {
            return Ok(None);
        }
        let mut plan = FusionPlan {
            nodes: Vec::new(),
            leaves: Vec::new(),
            steps: Vec::new(),
            reduction: None,
        };
        let name = symbol_name(head);
        let reduction = match &*name {
            "sum" => Some(Reduction::Sum),
            "any" => Some(Reduction::Any),
            "all" => Some(Reduction::All),
            _ => None,
        };
        // Any other root is an operator node or nothing, and a root that is not an operator
        // would make the whole tree one call operand.
        let root_operator = || {
            ARITHS.iter().any(|(_, operator)| *operator == name)
                || math_index(&name).is_some()
                || matches!(&*name, "(" | "&" | "|" | "!")
        };
        if reduction.is_none() && !root_operator() {
            return Ok(None);
        }
        if let Some(reduction) = reduction {
            let args = values::pairlist_items(call.cdr());
            let [(tag, argument)] = args.as_slice() else {
                return Ok(None);
            };
            let Some(expected) = crate::builtins::primitive(&name) else {
                return Ok(None);
            };
            if *tag != nil() || self.enclosing_function(head) != Some(expected) {
                return Ok(None);
            }
            plan.steps.push(FusionStep::Head(call, head, expected));
            if self.fusion_node(*argument, &mut plan)?.is_none() {
                return Ok(None);
            }
            plan.steps.push(FusionStep::Result(None));
            plan.reduction = Some(reduction);
        } else if self.fusion_node(call, &mut plan)?.is_none() {
            return Ok(None);
        }
        let operators = plan
            .nodes
            .iter()
            .filter(|node| !matches!(node, Node::Leaf(_)))
            .count()
            + usize::from(plan.reduction.is_some());
        Ok(
            (operators >= 2 && plan.nodes.len() <= MAX_NODES && plan.leaves.len() <= MAX_NODES)
                .then_some(plan),
        )
    }

    /// Adds `expression` to `plan` as a node of the element-wise tree and returns its index:
    /// a variable or numeric constant as a leaf, a call of a base operator or math function on
    /// such nodes as an operator node. `None` when it is anything else.
    unsafe fn fusion_node(&mut self, expression: Sexp, plan: &mut FusionPlan) -> R<Option<usize>> {
        match expression.kind() {
            SexpType::Symbol => {
                if expression == symbols().dots
                    || eval::dot_dot_index(expression).is_some()
                    || expression == missing_arg()
                {
                    return Ok(None);
                }
                plan.steps.push(FusionStep::Variable(plan.leaves.len()));
                return Ok(Some(plan.leaf(expression)));
            }
            SexpType::Real | SexpType::Integer | SexpType::Logical
                if expression.length() == 1 && expression.attrib() == nil() =>
            {
                return Ok(Some(plan.leaf(expression)));
            }
            SexpType::Language => {}
            _ => return Ok(None),
        }
        let head = expression.car();
        if plan.nodes.len() >= MAX_NODES {
            return Ok(None);
        }
        if head.kind() != SexpType::Symbol {
            return Ok(plan.call_leaf(expression));
        }
        let name = symbol_name(head);
        // The operators are all short and start with punctuation, which rules out the table scan
        // for most of the calls the compiler sees.
        let operator = name.len() <= 3
            && name
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_punctuation());
        let arith = operator
            .then(|| ARITHS.iter().find(|(_, operator)| *operator == name))
            .flatten()
            .map(|(arith, _)| *arith);
        let math = if operator { None } else { math_index(&name) };
        if arith.is_none() && math.is_none() && !matches!(&*name, "(" | "&" | "|" | "!") {
            return Ok(plan.call_leaf(expression));
        }
        let args = values::pairlist_items(expression.cdr());
        if args.iter().any(|(tag, value)| {
            *tag != nil() || *value == symbols().dots || *value == missing_arg()
        }) {
            return Ok(plan.call_leaf(expression));
        }
        let expected = match (&*name, args.len()) {
            ("(", 1) => self.base_primitive(head, &name)?,
            (_, 2) if arith.is_some() => self.base_primitive(head, &name)?,
            ("&" | "|", 2) | ("-" | "!", 1) => crate::builtins::primitive(&name)
                .filter(|&function| self.enclosing_function(head) == Some(function)),
            (_, 1) if math.is_some() => crate::builtins::primitive(&name)
                .filter(|&function| self.enclosing_function(head) == Some(function)),
            _ => None,
        };
        let Some(expected) = expected else {
            return Ok(plan.call_leaf(expression));
        };
        plan.steps
            .push(FusionStep::Head(expression, head, expected));
        let mut inputs = [0; 2];
        for (input, (_, argument)) in inputs.iter_mut().zip(&args) {
            match self.fusion_node(*argument, plan)? {
                Some(index) => *input = index,
                None => return Ok(None),
            }
        }
        let [first, second] = inputs;
        let node = match (&*name, args.len(), arith, math) {
            ("(", 1, _, _) => return Ok(Some(first)),
            (_, 2, Some(arith), _) => Node::Binary(arith, first, second),
            ("&" | "|", 2, _, _) => Node::Logic {
                is_and: name == "&",
                left: first,
                right: second,
            },
            ("-", 1, _, _) => Node::Negate(first),
            ("!", 1, _, _) => Node::Not(first),
            (_, 1, _, Some(math)) => Node::Math(MATHS[math].0, first),
            _ => return Ok(None),
        };
        plan.steps.push(FusionStep::Result(Some(plan.nodes.len())));
        plan.nodes.push(node);
        Ok(Some(plan.nodes.len() - 1))
    }

    /// The program [`Op::Fused`] runs for `plan`, laid out as [`fusion`] describes, with each
    /// step matched to the instruction of the ordinary code at `sites`. `None` when the ordinary
    /// code took another shape than the plan expects, such as a fallback to the evaluator.
    unsafe fn fusion_program(
        &mut self,
        plan: &FusionPlan,
        sites: &[u32],
        ahead: &[(usize, u32)],
        assigned: bool,
    ) -> Option<Vec<i32>> {
        if sites.len() != plan.steps.len() {
            return None;
        }
        let mut guards = Vec::new();
        let mut leaves: Vec<[i32; fusion::LEAF]> = vec![[0; fusion::LEAF]; plan.leaves.len()];
        let mut node_sites = vec![0; plan.nodes.len()];
        let mut reduction_site = 0;
        for &(leaf, register) in ahead {
            leaves[leaf] = [2, register as i32, 0, 0];
        }
        for (step, &site) in plan.steps.iter().zip(sites) {
            match *step {
                FusionStep::Variable(leaf) | FusionStep::Call(leaf) if leaves[leaf][0] == 2 => {
                    continue;
                }
                _ => {}
            }
            if site == UNGUARDED {
                if matches!(step, FusionStep::Head(..)) {
                    continue;
                }
                return None;
            }
            let position = site as usize;
            let op = op_of(*self.code.get(position)?)?;
            let operands = self
                .code
                .get(position + 1..position + 1 + operand_count(op))?
                .to_vec();
            let operand = |index: usize| operands[index];
            match *step {
                FusionStep::Call(_) => return None,
                FusionStep::Head(_, symbol, expected) => {
                    let (symbol_operand, cache, probe) = match op {
                        Op::Guard => (operand(0), operand(2), operand(3)),
                        Op::GetFun => (operand(1), operand(2), 1),
                        _ => return None,
                    };
                    if self.constant_at(symbol_operand) != Some(symbol) {
                        return None;
                    }
                    let expected = self.constant(expected);
                    guards.push([symbol_operand, expected, cache, probe].map(|word| word as i32));
                }
                FusionStep::Variable(leaf) => {
                    if op != Op::GetVar || self.constant_at(operand(1)) != Some(plan.leaves[leaf]) {
                        return None;
                    }
                    leaves[leaf] = [1, operand(1) as i32, operand(2) as i32, site as i32];
                }
                FusionStep::Result(node) => match (node, op) {
                    (Some(node), Op::Arithmetic | Op::CallBuiltin) => node_sites[node] = site,
                    (None, Op::CallBuiltin) => reduction_site = site,
                    _ => return None,
                },
            }
        }
        for (words, &leaf) in leaves.iter_mut().zip(&plan.leaves) {
            if words[0] != 2 && leaf.kind() != SexpType::Symbol {
                *words = [0, self.constant(leaf) as i32, 0, 0];
            }
        }
        let reduction = match plan.reduction {
            None if assigned => fusion::ASSIGNED,
            None => -1,
            Some(Reduction::Sum) => 0,
            Some(Reduction::Any) => 1,
            Some(Reduction::All) => 2,
        };
        let mut words = vec![
            0,
            plan.nodes.len() as i32,
            plan.leaves.len() as i32,
            reduction,
            reduction_site as i32,
            guards.len() as i32,
        ];
        words.extend(guards.iter().flatten());
        words.extend(leaves.iter().flatten());
        for (node, &site) in plan.nodes.iter().zip(&node_sites) {
            let site = site as i32;
            let arith_index = |arith: Arith| arith as i32;
            words.extend_from_slice(&match *node {
                Node::Leaf(leaf) => [0, leaf as i32, 0, 0, 0],
                Node::Binary(arith, left, right) => {
                    [1, arith_index(arith), left as i32, right as i32, site]
                }
                Node::Logic {
                    is_and,
                    left,
                    right,
                } => [2, i32::from(is_and), left as i32, right as i32, site],
                Node::Negate(input) => [3, input as i32, 0, 0, site],
                Node::Not(input) => [4, input as i32, 0, 0, site],
                Node::Math(math, input) => [
                    5,
                    MATHS.iter().position(|(function, _)| *function == math)? as i32,
                    input as i32,
                    0,
                    site,
                ],
            });
        }
        Some(words)
    }

    fn constant_at(&self, index: u32) -> Option<Sexp> {
        let index = (index as usize).checked_sub(FIRST_CONSTANT)?;
        self.constants.get(index).copied()
    }

    unsafe fn unfused_call(&mut self, call: Sexp, dst: u32) -> R<()> {
        let head = call.car();
        if head.kind() == SexpType::Language {
            // A head that is itself a call, most often `pkg::f`, evaluates to the function and
            // then calls it like a named one, so its arguments become compiled promises: a
            // `function` argument (`purrr::map(x, function(v) ...)`) then shares one compiled
            // body across calls instead of each closure compiling and tiering up on its own.
            let call_constant = self.call_site(call)?;
            let mark = self.next_register;
            let function = self.allocate();
            self.expression(head, function)?;
            self.generic_call(dst, function, call_constant, None);
            self.release_to(mark);
            return Ok(());
        }
        if head.kind() != SexpType::Symbol {
            return self.fallback(call, dst);
        }
        let name = symbol_name(head).into_owned();
        if let Some(primitive) = self.base_primitive(head, &name)? {
            let args = values::pairlist_items(call.cdr());
            // Ahead of the dispatch-or-eval path `$` would otherwise take, so a record field
            // read gets the site's shape cache.
            if name == "$"
                && let [(subject_tag, subject), (field_tag, field)] = args.as_slice()
                && *subject_tag == nil()
                && *field_tag == nil()
                && field.kind() == SexpType::Symbol
                && *subject != symbols().dots
                && *subject != missing_arg()
            {
                return self.field(call, head, primitive, dst, *subject, *field);
            }
            match name.as_str() {
                "$" => return self.general_call(call, head, dst),
                ".ry_record" | ".ry_tuple" | ".ry_tag" => {
                    return match constructor(&name, &args) {
                        Some((items, layout, slots)) => {
                            self.construct(call, head, primitive, dst, items, layout, &slots)
                        }
                        None => self.general_call(call, head, dst),
                    };
                }
                ".ry_match" if supported_match(&args)? => {
                    return self.compiled_match(call, head, primitive, dst, &args);
                }
                // `quote(x)` is `x` itself, so a slot frame needs no environment for it, as
                // `identical(x, quote(expr = ))` in rlang's `is_missing` would otherwise.
                "quote" => {
                    return match args.as_slice() {
                        [(tag, quoted)] if *tag == nil() || symbol_name(*tag) == "expr" => {
                            self.quoted(call, head, primitive, dst, *quoted)
                        }
                        _ => self.general_call(call, head, dst),
                    };
                }
                "::" | ":::" => {
                    let literal = match args.as_slice() {
                        [(package_tag, package), (name_tag, symbol)]
                            if *package_tag == nil() && *name_tag == nil() =>
                        {
                            match (colon_literal(*package)?, colon_literal(*symbol)?) {
                                (Some(package), Some(symbol)) => Some((package, symbol)),
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    return match literal {
                        Some((package, symbol)) => self.namespace(
                            call,
                            head,
                            primitive,
                            dst,
                            package,
                            symbol,
                            name == "::",
                        ),
                        None => self.general_call(call, head, dst),
                    };
                }
                "missing" => {
                    return match args.as_slice() {
                        [(tag, symbol)]
                            if *tag == nil()
                                && symbol.kind() == SexpType::Symbol
                                && *symbol != missing_arg()
                                && eval::dot_dot_index(*symbol).is_none()
                                && self.bound.is_none() =>
                        {
                            self.missing(call, head, primitive, dst, *symbol)
                        }
                        _ => self.general_call(call, head, dst),
                    };
                }
                _ => {}
            }
            // Dispatch on an evaluated subject evaluates the other arguments in the frame.
            if self.bound.is_none() && self.eager_subject(primitive, &args)? {
                let item = name == "[["
                    && args.len() == 2
                    && args.iter().all(|(tag, _)| *tag == nil())
                    && constant_item(args[1].1);
                return self.subject_call(call, head, primitive, dst, &args, item);
            }
            let plain = args.iter().all(|(tag, _)| *tag == nil());
            let inlined = match (name.as_str(), args.len()) {
                ("(", 1) => plain && args[0].1 != symbols().dots,
                ("{", _) | ("if", 2 | 3) | ("while", 2) | ("repeat", 1) | ("return", 0 | 1) => {
                    plain
                }
                ("for", 3) => plain && args[0].1.kind() == SexpType::Symbol,
                ("break" | "next", 0) => !self.open_loops.is_empty(),
                ("<-" | "=" | "<<-", 2) => plain && args[0].1.kind() == SexpType::Symbol,
                ("&&" | "||", 2) => plain,
                ("switch", 1..) => switch_compiles(&args),
                ("function", 2..) => true,
                (".Internal", 1) => plain && compiled_internal(args[0].1)?.is_some(),
                ("local", 1) => plain && args[0].1 != symbols().dots && args[0].1 != missing_arg(),
                (operator, 2) if plain && ARITHS.iter().any(|(_, symbol)| *symbol == operator) => {
                    args.iter()
                        .all(|(_, value)| *value != missing_arg() && *value != symbols().dots)
                }
                _ => false,
            };
            if matches!(name.as_str(), "<-" | "=")
                && args.len() == 2
                && args.iter().all(|(tag, _)| *tag == nil())
                && let Some((kind, replacement, subscripts)) =
                    self.element_assignment_target(args[0].1)?
            {
                return self.element_assignment(
                    call,
                    head,
                    primitive,
                    replacement,
                    kind,
                    &subscripts,
                    dst,
                    &args,
                );
            }
            if matches!(name.as_str(), "<-" | "=")
                && args.len() == 2
                && args.iter().all(|(tag, _)| *tag == nil())
                && let Some(setter) = self.replacement_target(args[0].1)?
            {
                return self.replacement(call, head, primitive, setter, dst, &args);
            }
            if inlined {
                return self.guarded(call, head, primitive, dst, &name, &args);
            }
        } else if INLINED_BASE_INTERNALS.contains(&name.as_str())
            && let Some(inline) = self.inlined_internal(head, call)?
        {
            return self.internal(call, head, inline, dst);
        } else if let Some(function) = self.library_function(head)? {
            let args = values::pairlist_items(call.cdr());
            // Registers do not count as references, so the later arguments must not update an
            // earlier one in place, as for any builtin call.
            let plain = args.iter().all(|(tag, value)| {
                *tag == nil() && *value != symbols().dots && *value != missing_arg()
            }) && !args
                .iter()
                .skip(1)
                .any(|(_, value)| may_modify_in_place(*value));
            if plain {
                return self.library_call(call, head, function, dst, &args);
            }
        }
        self.general_call(call, head, dst)
    }

    /// For `x[i]`, `x[[i]]` or `x$name` as an assignment target: its [`ElementKind`], the base
    /// replacement function, and the index expressions (`"name"` for `x$name`), when `x` is a
    /// variable and each subscript is one the compiler can evaluate into a register
    /// ([`Self::lowerable_subscript`]).
    unsafe fn element_assignment_target(
        &mut self,
        target: Sexp,
    ) -> R<Option<(ElementKind, Sexp, SmallVec<[Sexp; 2]>)>> {
        if target.kind() != SexpType::Language || target.car().kind() != SexpType::Symbol {
            return Ok(None);
        }
        let kind = match &*symbol_name(target.car()) {
            "[" => ElementKind::Single,
            "[[" => ElementKind::Double,
            "$" => ElementKind::Dollar,
            _ => return Ok(None),
        };
        let parts = values::pairlist_items(target.cdr());
        let Some(((variable_tag, variable), subscripts)) = parts.split_first() else {
            return Ok(None);
        };
        if *variable_tag != nil()
            || variable.kind() != SexpType::Symbol
            || !(1..=2).contains(&subscripts.len())
            || subscripts.iter().any(|(tag, _)| *tag != nil())
            || (kind == ElementKind::Dollar && subscripts.len() != 1)
        {
            return Ok(None);
        }
        let index = &subscripts[0].1;
        if kind == ElementKind::Dollar {
            // `x$name` quotes its index; a one-string index is the same name.
            let name = match index.kind() {
                SexpType::Symbol if *index != symbols().dots && *index != missing_arg() => {
                    index.car()
                }
                SexpType::String
                    if length(*index) == 1
                        && !values::has_attributes(*index)
                        && index.element(0) != values::na_string() =>
                {
                    index.element(0)
                }
                _ => return Ok(None),
            };
            let index = values::scalar_string_charsxp(name)?;
            return Ok(self
                .base_primitive(values::install("$<-")?, "$<-")?
                .map(|primitive| (kind, primitive, smallvec![index])));
        }
        if !subscripts
            .iter()
            .all(|(_, index)| self.lowerable_subscript(*index, *variable))
        {
            return Ok(None);
        }
        let name = if kind == ElementKind::Double {
            "[[<-"
        } else {
            "[<-"
        };
        let replacement = values::install(name)?;
        Ok(self.base_primitive(replacement, name)?.map(|primitive| {
            (
                kind,
                primitive,
                subscripts.iter().map(|(_, index)| *index).collect(),
            )
        }))
    }

    /// For `f(x, ...) <- value` other than the element assignments: the replacement function's
    /// symbol, when `x` is a variable and the other arguments can be evaluated before the
    /// variable is read without anyone noticing.
    unsafe fn replacement_target(&mut self, target: Sexp) -> R<Option<Sexp>> {
        if target.kind() != SexpType::Language || target.car().kind() != SexpType::Symbol {
            return Ok(None);
        }
        let getter = symbol_name(target.car());
        // `x[i]` and `x[[i]]` with one or two plain subscripts take the element assignment;
        // the shapes it refuses, such as `x[i, j, ]` or `x[, j]`, replace through `[<-` here.
        let element = matches!(&*getter, "[" | "[[");
        if matches!(&*getter, "$" | "@") {
            return Ok(None);
        }
        let parts = values::pairlist_items(target.cdr());
        let Some(((variable_tag, variable), rest)) = parts.split_first() else {
            return Ok(None);
        };
        let constant = |value: Sexp| {
            matches!(
                value.kind(),
                SexpType::String | SexpType::Logical | SexpType::Nil
            ) && length(value) <= 1
                && !values::has_attributes(value)
        };
        if *variable_tag != nil()
            || variable.kind() != SexpType::Symbol
            || *variable == symbols().dots
            || eval::dot_dot_index(*variable).is_some()
            || !rest.iter().all(|(tag, value)| {
                *tag != symbols().value
                    && (constant(*value)
                        || (element && *value == missing_arg())
                        // A formal may itself be missing, which selects everything only
                        // when `[<-` sees the symbol, so it keeps the general assignment.
                        || (self.lowerable_subscript(*value, *variable)
                            && !(element && self.formals.contains(value))))
            })
        {
            return Ok(None);
        }
        Ok(Some(values::install(&format!("{getter}<-"))?))
    }

    /// `f(x, ...) <- value`, guarded by `<-`: the value, then the other arguments, evaluate
    /// into registers, and [`Op::Replace`] reads the variable, calls the replacement function
    /// and binds its result.
    unsafe fn replacement(
        &mut self,
        call: Sexp,
        head: Sexp,
        primitive: Sexp,
        setter: Sexp,
        dst: u32,
        args: &[(Sexp, Sexp)],
    ) -> R<()> {
        let target = args[0].1;
        let guard = self.guard(call, head, primitive)?;
        let rest = values::pairlist_items(target.cdr().cdr());
        let count = rest.len() as u32 + 2;
        let mark = self.next_register;
        let base = self.next_register;
        for _ in 0..count {
            self.allocate();
        }
        self.expression(args[1].1, base + count - 1)?;
        self.read_target_first(
            target.cdr().car(),
            rest.iter()
                .map(|(_, value)| *value)
                .filter(|value| *value != missing_arg()),
        )?;
        for (index, (_, value)) in rest.iter().enumerate() {
            let register = base + 1 + index as u32;
            if *value == missing_arg() {
                let empty = self.constant(missing_arg());
                self.emit(Op::Constant, &[register, empty]);
            } else {
                self.expression(*value, register)?;
            }
        }
        // The call the builtin sees, as GNU R builds it: `` `f<-`(`*tmp*`, ..., value = <rhs>) ``.
        let mut items: SmallVec<[(Sexp, Sexp); 6]> =
            smallvec![(nil(), setter), (nil(), symbols().tmp)];
        items.extend(rest.iter().copied());
        items.push((symbols().value, args[1].1));
        let setter_call = values::pairlist(&items, SexpType::Language)?;
        let variable = self.constant(target.cdr().car());
        let setter_symbol = self.constant(setter);
        let cache = self.cache();
        let setter_call = self.constant(setter_call);
        let whole = self.constant(call);
        self.emit(
            Op::Replace,
            &[
                dst,
                variable,
                setter_symbol,
                cache,
                setter_call,
                base,
                count,
                whole,
            ],
        );
        self.release_to(mark);
        self.cold_fallback(guard.as_slice(), call, dst)?;
        Ok(())
    }

    /// Reads `variable` into a scratch register when one of `subscripts` might observe or change
    /// it, so the assignment reads it before evaluating them, as GNU R's does: an unbound
    /// variable is the error, before anything a subscript prints or raises.
    unsafe fn read_target_first(
        &mut self,
        variable: Sexp,
        mut subscripts: impl Iterator<Item = Sexp>,
    ) -> R<()> {
        if subscripts.all(|subscript| self.side_effect_free_index(subscript, variable, 0)) {
            return Ok(());
        }
        let mark = self.next_register;
        let scratch = self.allocate();
        self.expression(variable, scratch)?;
        self.release_to(mark);
        Ok(())
    }

    /// Whether the compiled assignment to `target` can take `index` as an argument it evaluates
    /// into a register: anything but `...`, `..1` or an empty argument that assigns to `target`
    /// nowhere in its code. Such an assignment rebinds the variable while the replacement holds
    /// its old value, which [`eval::complex_assign`] handles by copying. The instruction that
    /// takes the registers hands them to the replacement as forced promises, so nothing is
    /// evaluated twice, and when `index` is not [side-effect free](Self::side_effect_free_index)
    /// the compiled code reads `target` before it, as GNU R does.
    unsafe fn lowerable_subscript(&self, index: Sexp, target: Sexp) -> bool {
        let symbol = index.kind() == SexpType::Symbol;
        !(symbol
            && (index == symbols().dots
                || index == missing_arg()
                || eval::dot_dot_index(index).is_some()))
            && !eval::assigns_to(index, target, symbols())
    }

    /// Whether evaluating the subscript `index` cannot change `target` or anything outside the
    /// frame, so the compiled assignment may evaluate it before reading `target`: a variable of
    /// the frame (a formal's promise runs in the caller's frame), a numeric constant, a name,
    /// base arithmetic and comparison on those, base `$` and `[[` reading one of those, `if`
    /// or parentheses over them, or an assignment of one of them to another local variable (the
    /// growth idiom `x[[cap <- if (cap < n) 2L * cap else cap]] <- v`).
    unsafe fn side_effect_free_index(&self, index: Sexp, target: Sexp, depth: u32) -> bool {
        match index.kind() {
            SexpType::Symbol => {
                index != symbols().dots
                    && index != missing_arg()
                    && eval::dot_dot_index(index).is_none()
                    && (self.formals.contains(&index)
                        || self
                            .local_symbols
                            .as_ref()
                            .is_some_and(|found| found.contains(&index)))
            }
            SexpType::Integer | SexpType::Real => {
                length(index) == 1 && !values::has_attributes(index)
            }
            // A name is a subscript, but an operand of arithmetic would be an error.
            SexpType::String => depth == 0 && length(index) == 1 && !values::has_attributes(index),
            SexpType::Language if depth < 8 && index.car().kind() == SexpType::Symbol => {
                let operator = symbol_name(index.car());
                let arguments = values::pairlist_items(index.cdr());
                let name = |value: Sexp| {
                    value.kind() == SexpType::String
                        && length(value) == 1
                        && !values::has_attributes(value)
                };
                if let ("$" | "[[", [(nil_tag, object), (key_tag, key)]) =
                    (&*operator, arguments.as_slice())
                {
                    let key_free = match &*operator {
                        "$" => key.kind() == SexpType::Symbol && *key != symbols().dots,
                        _ => name(*key) || self.side_effect_free_index(*key, target, depth + 1),
                    };
                    return *nil_tag == nil()
                        && *key_tag == nil()
                        && key_free
                        && !self.frame_may_bind(index.car())
                        && self.side_effect_free_index(*object, target, depth + 1);
                }
                if arguments.iter().any(|(tag, _)| *tag != nil())
                    || self.frame_may_bind(index.car())
                {
                    return false;
                }
                let operand =
                    |argument: Sexp| self.side_effect_free_index(argument, target, depth + 1);
                match (&*operator, arguments.as_slice()) {
                    ("<-" | "=", [(_, variable), (_, value)]) => {
                        variable.kind() == SexpType::Symbol
                            && *variable != target
                            && self.side_effect_free_index(*variable, target, depth + 1)
                            && operand(*value)
                    }
                    ("if", [(_, condition), (_, yes), (_, no)]) => {
                        operand(*condition) && operand(*yes) && operand(*no)
                    }
                    ("(", [(_, inner)]) => operand(*inner),
                    _ => {
                        ARITHS.iter().any(|(_, name)| *name == &*operator)
                            && (1..=2).contains(&arguments.len())
                            && arguments.iter().all(|(_, argument)| operand(*argument))
                    }
                }
            }
            _ => false,
        }
    }

    /// `x[i] <- value`, guarded by both `<-` and the replacement function.
    #[allow(clippy::too_many_arguments)]
    unsafe fn element_assignment(
        &mut self,
        call: Sexp,
        head: Sexp,
        primitive: Sexp,
        replacement: Sexp,
        kind: ElementKind,
        subscripts: &[Sexp],
        dst: u32,
        args: &[(Sexp, Sexp)],
    ) -> R<()> {
        let target = args[0].1;
        let replacement_symbol = values::install(match kind {
            ElementKind::Single => "[<-",
            ElementKind::Double => "[[<-",
            ElementKind::Dollar => "$<-",
        })?;
        let mut guards = Vec::new();
        for (symbol, expected) in [(head, primitive), (replacement_symbol, replacement)] {
            guards.extend(self.guard(call, symbol, expected)?);
        }
        let mark = self.next_register;
        let value = self.allocate();
        self.expression(args[1].1, value)?;
        self.read_target_first(target.cdr().car(), subscripts.iter().copied())?;
        let mut registers: SmallVec<[u32; 2]> = SmallVec::new();
        for &subscript in subscripts {
            let register = self.allocate();
            if self.formals.contains(&subscript) {
                // A missing argument selects everything, which only the general assignment
                // does, so the instruction looks the formal up itself (see `subscript_value`).
                let symbol = self.constant(subscript);
                self.emit(Op::Constant, &[register, symbol]);
            } else {
                self.expression(subscript, register)?;
            }
            registers.push(register);
        }
        let variable = self.constant(target.cdr().car());
        let call_constant = self.constant(call);
        match registers[..] {
            [row, column] => self.emit(
                Op::SubAssignMatrix,
                &[dst, variable, row, column, value, call_constant],
            ),
            [index, ..] => {
                // The replacement call `complex_assign` would make, `` `[<-`(`*tmp*`, i,
                // value = v) ``, which errors from the primitive name as GNU R's do.
                let mut items: SmallVec<[(Sexp, Sexp); 4]> =
                    smallvec![(nil(), replacement_symbol), (nil(), symbols().tmp)];
                let mut rest = target.cdr().cdr();
                while matches!(rest.kind(), SexpType::Pairlist | SexpType::Language) {
                    items.push((rest.tag(), rest.car()));
                    rest = rest.cdr();
                }
                items.push((symbols().value, args[1].1));
                let setter_call = values::pairlist(&items, SexpType::Language)?;
                let setter_call = self.constant(setter_call);
                self.emit(
                    Op::SubAssign,
                    &[
                        dst,
                        variable,
                        index,
                        value,
                        call_constant,
                        kind as u32,
                        setter_call,
                    ],
                )
            }
            [] => self.evaluate(dst, call_constant),
        }
        self.release_to(mark);
        self.cold_fallback(&guards, call, dst)?;
        Ok(())
    }

    /// The base primitive `symbol` names from the closure's enclosure, if it is one the compiler
    /// knows how to inline.
    unsafe fn base_primitive(&mut self, symbol: Sexp, name: &str) -> R<Option<Sexp>> {
        let inlinable = matches!(
            name,
            "{" | "("
                | "if"
                | "for"
                | "while"
                | "repeat"
                | "break"
                | "next"
                | "return"
                | "<-"
                | "="
                | "<<-"
                | "&&"
                | "||"
                | "function"
                | "["
                | "[["
                | "[<-"
                | "[[<-"
                | "$<-"
                | ".Internal"
                | "local"
                | "$"
                | ".ry_record"
                | ".ry_tuple"
                | ".ry_tag"
                | ".ry_match"
                | "switch"
                | "missing"
                | "quote"
                | "::"
                | ":::"
        ) || ARITHS.iter().any(|(_, operator)| *operator == name);
        if !inlinable {
            return Ok(None);
        }
        let expected = if name == "local" {
            // `local` is a closure, so the function to expect is base's own binding, which stays
            // a lazy-load promise until something forces it.
            let heap = crate::heap();
            heap.frame_cell(heap.known().base_env, symbol)
                .map(|cell| forced_value(cell.cdr()))
                .filter(|value| *value != unbound())
        } else {
            crate::builtins::primitive(name)
        };
        let Some(expected) = expected else {
            return Ok(None);
        };
        Ok(self
            .enclosing_function(symbol)
            .filter(|&value| value == expected))
    }

    /// The function `symbol` names from the closure's enclosure, found by a walk that forces no
    /// promise, since compiling must not run code. `None` when a binding on the way is active
    /// or an unforced promise.
    unsafe fn enclosing_function(&self, symbol: Sexp) -> Option<Sexp> {
        let heap = crate::heap();
        let mut env = self.enclosure;
        while env != heap.known().empty_env {
            if let Some(cell) = heap.frame_cell(env, symbol) {
                let value = forced_value(cell.cdr());
                if cell.has_gp(gp::ACTIVE_BINDING) || value.kind() == SexpType::Promise {
                    return None;
                }
                if values::is_function(value) {
                    return Some(value);
                }
            }
            env = env.enclosure();
        }
        None
    }

    /// For a call of base's closure `head` whose body is only `.Internal(f(...))`, the closure,
    /// the internal and the internal's arguments, as GNU R's compiler inlines such a "safe base
    /// internal": each argument is the caller's expression for the formal it names, or the
    /// formal's default, or a literal. Every default must be a constant, and each formal may
    /// appear once, so evaluating the arguments in the internal's order forces them as the call
    /// would. A `...` formal takes the arguments no other formal matches, as R matches them, and
    /// the body may pass it on as `...` or as `list(...)`: `sprintf` and `paste` do. `None`
    /// otherwise, for the ordinary call.
    unsafe fn inlined_internal(&self, head: Sexp, call: Sexp) -> R<Option<InlinedInternal>> {
        let heap = crate::heap();
        let Some(binding) = heap
            .frame_cell(heap.known().base_env, head)
            .map(|cell| cell.cdr())
        else {
            return Ok(None);
        };
        // Base's closures start as lazy-load promises, and a loop body that calls one is
        // compiled before the first call forces it. Fetching a closure record runs no R code,
        // so it is forced here as GNU R's compiler forces it to read the definition.
        const CLOSURE_RECORD: u32 = 3;
        if let Some((CLOSURE_RECORD, false, _)) = crate::serialize::lazy_record_header(binding)? {
            eval::force_promise(binding)?;
        }
        let function = forced_value(binding);
        if function.kind() != SexpType::Closure || self.enclosing_function(head) != Some(function) {
            return Ok(None);
        }
        let mut body = closure_body(function);
        if body.kind() == SexpType::Language
            && body.car() == symbols().brace
            && values::pairlist_length(body.cdr()) == 1
        {
            body = body.cdr().car();
        }
        if body.kind() != SexpType::Language || body.car() != symbols().dot_internal {
            return Ok(None);
        }
        let inner = body.cdr().car();
        if inner.kind() != SexpType::Language || inner.car().kind() != SexpType::Symbol {
            return Ok(None);
        }
        let Some(internal) = crate::builtins::internal_function(inner.car()) else {
            return Ok(None);
        };
        // A special internal takes its arguments unevaluated, which inlining would change.
        if eval::builtin_def(internal)?.kind != crate::builtins::Kind::Internal {
            return Ok(None);
        }
        let dots_symbol = symbols().dots;
        let formals = values::pairlist_items(function.car());
        let constant = |value: Sexp| {
            !matches!(
                value.kind(),
                SexpType::Symbol | SexpType::Language | SexpType::Promise | SexpType::Bytecode
            )
        };
        if formals.len() > 64
            || formals.iter().any(|&(tag, default)| {
                tag != dots_symbol && !(default == missing_arg() || constant(default))
            })
        {
            return Ok(None);
        }
        let dots_index = formals.iter().position(|(tag, _)| *tag == dots_symbol);
        // Positions fill the formals before `...`, and a tag matches only those partially.
        let positional = dots_index.unwrap_or(formals.len());
        let mut supplied: SmallVec<[Option<Sexp>; 8]> = SmallVec::from_elem(None, formals.len());
        let mut dots: SmallVec<[(Sexp, Sexp); 6]> = SmallVec::new();
        let args = values::pairlist_items(call.cdr());
        let exact = |tag: Sexp| {
            formals
                .iter()
                .position(|(formal, _)| *formal == tag && tag != dots_symbol)
        };
        for (tag, value) in &args {
            if *value == dots_symbol || *value == missing_arg() || *tag == dots_symbol {
                return Ok(None);
            }
            if *tag == nil() {
                continue;
            }
            if let Some(index) = exact(*tag) {
                if supplied[index].replace(*value).is_some() {
                    return Ok(None);
                }
                continue;
            }
            let name = symbol_name(*tag);
            if dots_index.is_none()
                || formals[..positional]
                    .iter()
                    .any(|(formal, _)| symbol_name(*formal).starts_with(&*name))
            {
                return Ok(None);
            }
        }
        for (tag, value) in &args {
            if *tag != nil() {
                if exact(*tag).is_none() {
                    dots.push((*tag, *value));
                }
                continue;
            }
            match supplied[..positional]
                .iter_mut()
                .find(|slot| slot.is_none())
            {
                Some(slot) => *slot = Some(*value),
                None if dots_index.is_some() => dots.push((nil(), *value)),
                None => return Ok(None),
            }
        }
        let mut used = 0u64;
        let mut use_formal = |index: usize| {
            let fresh = used & 1 << index == 0;
            used |= 1 << index;
            fresh
        };
        let mut arguments = SmallVec::new();
        for (tag, argument) in values::pairlist_items(inner.cdr()) {
            if tag != nil() {
                return Ok(None);
            }
            if argument == dots_symbol {
                // The internal reads `...` as its own arguments, without their tags.
                let Some(index) = dots_index else {
                    return Ok(None);
                };
                if !use_formal(index) || dots.iter().any(|(tag, _)| *tag != nil()) {
                    return Ok(None);
                }
                arguments.extend(
                    dots.iter()
                        .map(|&(_, value)| InternalArgument::Value(value)),
                );
            } else if argument.kind() == SexpType::Language
                && argument.car() == symbols().list
                && argument.cdr().kind() == SexpType::Pairlist
                && argument.cdr().car() == dots_symbol
                && argument.cdr().tag() == nil()
                && argument.cdr().cdr() == nil()
            {
                let Some(index) = dots_index else {
                    return Ok(None);
                };
                // The body's `list` is base's, as the closure's environment is base's namespace.
                let list = heap
                    .frame_cell(heap.known().base_env, symbols().list)
                    .map(|cell| forced_value(cell.cdr()));
                if !use_formal(index)
                    || list.is_none_or(|list| {
                        list.kind() != SexpType::Builtin || values::primitive_name(list) != "list"
                    })
                {
                    return Ok(None);
                }
                arguments.push(InternalArgument::List);
            } else if argument.kind() == SexpType::Symbol {
                let Some(index) = formals.iter().position(|(formal, _)| *formal == argument) else {
                    return Ok(None);
                };
                if Some(index) == dots_index || !use_formal(index) {
                    return Ok(None);
                }
                arguments.push(InternalArgument::Value(match supplied[index] {
                    Some(value) => value,
                    None if formals[index].1 != missing_arg() => formals[index].1,
                    None => return Ok(None),
                }));
            } else if constant(argument) && argument != missing_arg() {
                arguments.push(InternalArgument::Value(argument));
            } else {
                return Ok(None);
            }
        }
        // Arguments the body never passes on would be dropped unevaluated.
        if dots_index.is_some_and(|index| used & 1 << index == 0) {
            return Ok(None);
        }
        Ok(Some(InlinedInternal {
            function,
            internal,
            arguments,
            dots,
        }))
    }

    /// A call as [`Compiler::inlined_internal`] found it: a guard that `head` still names base's
    /// closure, the internal's arguments evaluated into a run of registers, and
    /// [`Op::Internal`]. A `list(...)` argument is built by base's `list` from the forwarded
    /// arguments, with their tags.
    unsafe fn internal(
        &mut self,
        call: Sexp,
        head: Sexp,
        inline: InlinedInternal,
        dst: u32,
    ) -> R<()> {
        let guard = self.guard(call, head, inline.function)?;
        let mark = self.next_register;
        let first = self.next_register;
        for _ in &inline.arguments {
            self.allocate();
        }
        for (offset, argument) in inline.arguments.iter().enumerate() {
            let register = first + offset as u32;
            match *argument {
                InternalArgument::Value(expression) => self.expression(expression, register)?,
                InternalArgument::List => {
                    let heap = crate::heap();
                    let Some(list) = heap
                        .frame_cell(heap.known().base_env, symbols().list)
                        .map(|cell| forced_value(cell.cdr()))
                    else {
                        return Err(error("base's 'list' is not bound"));
                    };
                    let values: SmallVec<[Sexp; 6]> =
                        inline.dots.iter().map(|&(_, value)| value).collect();
                    let list_call = values::call(symbols().list, &values)?;
                    let list_call_constant = self.constant(list_call);
                    let mut cell = list_call.cdr();
                    for &(tag, _) in &inline.dots {
                        heap.set_tag(cell, tag);
                        cell = cell.cdr();
                    }
                    let list_mark = self.next_register;
                    let function_register = self.allocate();
                    let list_constant = self.constant(list);
                    self.emit(Op::Constant, &[function_register, list_constant]);
                    let base = self.next_register;
                    for _ in &inline.dots {
                        self.allocate();
                    }
                    for (index, &(_, value)) in inline.dots.iter().enumerate() {
                        self.expression(value, base + index as u32)?;
                    }
                    self.emit(
                        Op::CallBuiltin,
                        &[
                            register,
                            function_register,
                            list_call_constant,
                            base,
                            inline.dots.len() as u32,
                        ],
                    );
                    self.release_to(list_mark);
                }
            }
        }
        let call_constant = self.constant(call);
        let internal = self.constant(inline.internal);
        self.emit(
            Op::Internal,
            &[
                dst,
                call_constant,
                internal,
                first,
                inline.arguments.len() as u32,
            ],
        );
        self.release_to(mark);
        self.cold_fallback(guard.as_slice(), call, dst)
    }

    /// The standard library builtin `symbol` names from the closure's enclosure, such as
    /// `Dict.get` from `package:ry`.
    unsafe fn library_function(&self, symbol: Sexp) -> R<Option<Sexp>> {
        let Some(function) = self.enclosing_function(symbol) else {
            return Ok(None);
        };
        if function.kind() != SexpType::Builtin {
            return Ok(None);
        }
        let definition = eval::builtin_def(function)?;
        Ok(
            (definition.accepts_native && definition.kind == crate::builtins::Kind::Builtin)
                .then_some(function),
        )
    }

    /// A call to a standard library builtin resolved while compiling: a guard that the name
    /// still finds that builtin, then the builtin called on the argument values, with no lookup
    /// of the function, no promises and no dispatch. A name rebound since compiling falls back
    /// to the evaluator.
    unsafe fn library_call(
        &mut self,
        call: Sexp,
        head: Sexp,
        function: Sexp,
        dst: u32,
        args: &[(Sexp, Sexp)],
    ) -> R<()> {
        let guard = self.guard(call, head, function)?;
        let call_constant = self.constant(call);
        let mark = self.next_register;
        let function_register = self.allocate();
        let expected = self.constant(function);
        self.emit(Op::Constant, &[function_register, expected]);
        if self.tail_updates.0.contains(&call) {
            let target = self.constant(self.tail_updates.1);
            self.emit(
                Op::CallTailUpdate,
                &[dst, function_register, call_constant, target],
            );
            self.release_to(mark);
            self.cold_fallback(guard.as_slice(), call, dst)?;
            return Ok(());
        }
        let rebinding = match self.rebinding {
            Some((value, target)) if value == call => {
                self.rebinding = None;
                eval::builtin_def(function)?
                    .reuses_first_argument
                    .then(|| self.constant(target))
            }
            _ => None,
        };
        let base = self.next_register;
        for _ in args {
            self.allocate();
        }
        for (index, (_, value)) in args.iter().enumerate() {
            self.expression(*value, base + index as u32)?;
        }
        let count = args.len() as u32;
        match rebinding {
            Some(target) => self.emit(
                Op::CallBuiltinRebinding,
                &[dst, function_register, call_constant, base, count, target],
            ),
            None => self.emit(
                Op::CallBuiltin,
                &[dst, function_register, call_constant, base, count],
            ),
        };
        self.release_to(mark);
        self.cold_fallback(guard.as_slice(), call, dst)?;
        Ok(())
    }

    unsafe fn guarded(
        &mut self,
        call: Sexp,
        head: Sexp,
        primitive: Sexp,
        dst: u32,
        name: &str,
        args: &[(Sexp, Sexp)],
    ) -> R<()> {
        // An operator a fused expression checked before evaluating a hoisted operand must not
        // fall back to the evaluator, which would evaluate that operand a second time.
        if self
            .preguarded
            .iter()
            .any(|(operation, _)| *operation == call)
        {
            self.inline(call, primitive, dst, name, args)?;
        } else {
            let guard = self.guard(call, head, primitive)?;
            self.inline(call, primitive, dst, name, args)?;
            self.cold_fallback(guard.as_slice(), call, dst)?;
        }
        if ARITHS.iter().any(|(_, operator)| *operator == name) {
            self.temporary = Some((dst, self.here()));
        }
        Ok(())
    }

    /// `missing(symbol)`, guarded by `missing`: [`Op::Missing`] reads the frame's binding of
    /// `symbol`, which is what the special does once it has matched its one argument.
    unsafe fn missing(
        &mut self,
        call: Sexp,
        head: Sexp,
        primitive: Sexp,
        dst: u32,
        symbol: Sexp,
    ) -> R<()> {
        let guard = self.guard(call, head, primitive)?;
        let symbol = self.constant(symbol);
        self.emit(Op::Missing, &[dst, symbol]);
        self.cold_fallback(guard.as_slice(), call, dst)
    }

    /// `package::symbol` or `package:::symbol`, guarded by the operator: [`Op::Namespace`].
    #[allow(clippy::too_many_arguments)]
    unsafe fn namespace(
        &mut self,
        call: Sexp,
        head: Sexp,
        primitive: Sexp,
        dst: u32,
        package: Sexp,
        symbol: Sexp,
        exported: bool,
    ) -> R<()> {
        let guard = self.guard(call, head, primitive)?;
        let package = self.constant(package);
        let symbol = self.constant(symbol);
        let lookup = self.cache();
        self.emit(
            Op::Namespace,
            &[dst, package, symbol, u32::from(exported), lookup],
        );
        self.cold_fallback(guard.as_slice(), call, dst)
    }

    /// `quote(quoted)`, guarded by `quote`: the quoted expression as a constant.
    unsafe fn quoted(
        &mut self,
        call: Sexp,
        head: Sexp,
        primitive: Sexp,
        dst: u32,
        quoted: Sexp,
    ) -> R<()> {
        let guard = self.guard(call, head, primitive)?;
        let constant = self.constant(quoted);
        self.emit(Op::Constant, &[dst, constant]);
        self.emit(Op::Visible, &[]);
        self.cold_fallback(guard.as_slice(), call, dst)
    }

    unsafe fn inline(
        &mut self,
        call: Sexp,
        primitive: Sexp,
        dst: u32,
        name: &str,
        args: &[(Sexp, Sexp)],
    ) -> R<()> {
        match name {
            "{" => {
                if args.is_empty() {
                    let constant = self.constant(nil());
                    self.emit(Op::Constant, &[dst, constant]);
                    // `{}` is visible NULL.
                    self.emit(Op::Visible, &[]);
                }
                let srcrefs = values::get_attr(call, symbols().srcref);
                let outer = self.srcref;
                let start = self.next_register;
                let mut used = start;
                for (index, (_, statement)) in args.iter().enumerate() {
                    // Registers are allocated in stack order, so the statement's temporaries
                    // are exactly `start..used`.
                    if index > 0 {
                        self.emit(Op::EndStatement, &[dst, start, used]);
                    }
                    let peak = self.registers;
                    self.registers = start;
                    if srcrefs != nil() {
                        self.srcref = eval::statement_srcref(srcrefs, index + 1);
                        let srcref = self.constant(self.srcref);
                        self.emit(Op::SetSrcref, &[srcref, 1]);
                    }
                    self.expression(*statement, dst)?;
                    used = self.registers;
                    self.registers = peak.max(used);
                }
                if srcrefs == nil() {
                    return Ok(());
                }
                self.srcref = outer;
                let srcref = self.constant(outer);
                // Restoring the outer srcref after a block starts no statement, so the
                // debugger must not stop on it.
                self.emit(Op::SetSrcref, &[srcref, 0]);
            }
            "(" => {
                self.expression(args[0].1, dst)?;
                self.emit(Op::Visible, &[]);
            }
            ".Internal" => {
                let inner = args[0].1;
                let Some(function) = compiled_internal(inner)? else {
                    return self.fallback(call, dst);
                };
                let inner_args = values::pairlist_items(inner.cdr());
                if eval::builtin_def(function)?.prologue && inner_args.is_empty() && !self.argument
                {
                    let function = self.constant(function);
                    let inner = self.constant(inner);
                    self.emit(Op::Prologue, &[dst, function, inner]);
                    return Ok(());
                }
                let mark = self.next_register;
                let function_register = self.allocate();
                let constant = self.constant(function);
                self.emit(Op::Constant, &[function_register, constant]);
                let base = self.next_register;
                for _ in &inner_args {
                    self.allocate();
                }
                for (index, (_, value)) in inner_args.iter().enumerate() {
                    self.expression(*value, base + index as u32)?;
                }
                let inner_constant = self.constant(inner);
                self.emit(
                    Op::CallBuiltin,
                    &[
                        dst,
                        function_register,
                        inner_constant,
                        base,
                        inner_args.len() as u32,
                    ],
                );
                self.release_to(mark);
            }
            "if" => {
                let call_constant = self.constant(call);
                self.expression(args[0].1, dst)?;
                let skip = self.emit_patchable(Op::JumpUnless, &[dst, call_constant, 0]);
                self.expression(args[1].1, dst)?;
                let done = self.emit_patchable(Op::Jump, &[0]);
                self.patch(skip);
                if let Some((_, alternative)) = args.get(2) {
                    self.expression(*alternative, dst)?;
                } else {
                    let constant = self.constant(nil());
                    self.emit(Op::Constant, &[dst, constant]);
                    self.emit(Op::Invisible, &[]);
                }
                self.patch(done);
            }
            "switch" => {
                self.expression(args[0].1, dst)?;
                let (table, arms) = switch_table(call, &args[1..])?;
                let mut numbers = Vec::with_capacity(arms.len() + 1);
                for arm in 0..=arms.len() {
                    numbers.push(self.constant(values::scalar_integer(arm as i32)?));
                }
                for (slot, &number) in table
                    .element(SWITCH_NUMBERS)
                    .slice_mut::<i32>()
                    .iter_mut()
                    .zip(&numbers)
                {
                    *slot = number as i32;
                }
                let table = self.constant(table);
                self.emit(Op::SwitchArm, &[dst, dst, table]);
                let mut done = Vec::with_capacity(arms.len());
                for (index, body) in arms.iter().enumerate() {
                    let number = numbers[index + 1];
                    let skip = self.emit_patchable(Op::BranchUnlessSame, &[dst, number, 0]);
                    self.expression(*body, dst)?;
                    done.push(self.emit_patchable(Op::Jump, &[0]));
                    self.patch(skip);
                }
                let constant = self.constant(nil());
                self.emit(Op::Constant, &[dst, constant]);
                self.emit(Op::Invisible, &[]);
                for position in done {
                    self.patch(position);
                }
            }
            "for" => self.for_loop(call, args, dst)?,
            "while" => {
                let call_constant = self.constant(call);
                let loop_index = self.open_loop();
                let top = self.here();
                self.loops[loop_index].step = top;
                self.expression(args[0].1, dst)?;
                let exit = self.emit_patchable(Op::JumpUnless, &[dst, call_constant, 0]);
                self.expression(args[1].1, dst)?;
                self.back_edge(top);
                self.patch(exit);
                self.close_loop(loop_index);
                self.loop_value(dst);
            }
            "repeat" => {
                let loop_index = self.open_loop();
                let top = self.here();
                self.loops[loop_index].step = top;
                self.expression(args[0].1, dst)?;
                self.back_edge(top);
                self.close_loop(loop_index);
                self.loop_value(dst);
            }
            "break" | "next" => {
                let (loop_index, _) = *self
                    .open_loops
                    .last()
                    .ok_or_else(|| error("no loop for break/next"))?;
                // The targets are known once the loop closes; record the operand to patch.
                let position = self.emit_patchable(Op::Jump, &[0]);
                let marker = if name == "break" {
                    BREAK_MARKER
                } else {
                    NEXT_MARKER
                };
                self.code[position] = marker | loop_index as u32;
            }
            "return" => {
                match args.first() {
                    Some((_, value)) => self.expression(*value, dst)?,
                    None => {
                        let constant = self.constant(nil());
                        self.emit(Op::Constant, &[dst, constant]);
                    }
                }
                self.emit(Op::Return, &[dst]);
            }
            // A slot frame's `CallRebinding` decides from the slot whether the builtin may update
            // in place; a frame with an environment leaves that to `eval::rebind`.
            "<-" | "=" if reuses_first_argument(call) && self.slot_formals.is_none() => {
                self.fallback(call, dst)?
            }
            "<-" | "=" => {
                if rebinds_first_argument(call) {
                    self.rebinding = Some((args[1].1, args[0].1));
                }
                if assigns_fusible(call) {
                    self.assigning = Some(args[1].1);
                }
                let compiled = self.expression(args[1].1, dst);
                self.rebinding = None;
                self.assigning = None;
                compiled?;
                let symbol = self.constant(args[0].1);
                let slot = self.cache();
                self.emit(Op::SetVar, &[symbol, dst, slot]);
            }
            "<<-" => {
                self.expression(args[1].1, dst)?;
                let symbol = self.constant(args[0].1);
                self.emit(Op::SuperAssign, &[symbol, dst]);
            }
            "&&" | "||" => {
                let is_and = (name == "&&") as u32;
                let call_constant = self.constant(call);
                let left = self.allocate();
                self.expression(args[0].1, left)?;
                let decided =
                    self.emit_patchable(Op::LogicLeft, &[dst, left, call_constant, is_and, 0]);
                let mark = self.next_register;
                let right = self.allocate();
                self.expression(args[1].1, right)?;
                self.emit(Op::LogicRight, &[dst, left, right, call_constant, is_and]);
                self.release_to(mark);
                self.patch(decided);
                self.release_to(left);
            }
            "local" => {
                // As GNU R's compiler does, `local(expr)` runs as `(function() expr)()`: a call
                // of a closure made on the spot, instead of `local`'s `eval.parent(substitute(
                // eval(quote(expr), new.env())))`, which costs several calls and an `eval`.
                let expression = args[0].1;
                // The `function` call carries the srcref slot, NULL here, as the parser's does:
                // rlang's `trace_back` zaps a frame call's `function` head by poking that slot.
                let function_call = values::lcons(
                    symbols().function,
                    values::cons(
                        nil(),
                        values::cons(expression, values::cons(nil(), nil())?)?,
                    )?,
                )?;
                let immediate_call = values::lcons(function_call, nil())?;
                let mark = self.next_register;
                let function = self.allocate();
                let body = self.nested_body(nil(), expression, nil())?;
                let formals = self.constant(nil());
                let srcref = self.constant(nil());
                self.emit(Op::Closure, &[function, formals, body, srcref]);
                let call_constant = self.call_site(immediate_call)?;
                self.emit(Op::CallGeneric, &[dst, function, call_constant]);
                self.release_to(mark);
            }
            "function" => {
                let srcref = args.get(2).map_or(nil(), |(_, srcref)| *srcref);
                let body = self.nested_body(args[0].1, args[1].1, srcref)?;
                let formals = self.constant(args[0].1);
                let srcref = self.constant(srcref);
                self.emit(Op::Closure, &[dst, formals, body, srcref]);
            }
            operator => {
                let arith = ARITHS
                    .iter()
                    .find(|(_, symbol)| *symbol == operator)
                    .map(|(arith, _)| *arith)
                    .ok_or_else(|| error("unknown operator"))?;
                if args.len() == 2 && may_modify_in_place(args[1].1) {
                    return self.fallback(call, dst);
                }
                let mark = self.next_register;
                let left = self.allocate();
                let right = self.allocate();
                self.expression(args[0].1, left)?;
                let left_temporary = self.temporary == Some((left, self.here()));
                self.expression(args[1].1, right)?;
                let right_temporary = self.temporary == Some((right, self.here()));
                let reuse = u32::from(left_temporary) | u32::from(right_temporary) << 1;
                let call_constant = self.constant(call);
                let function = self.constant(primitive);
                self.emit(
                    Op::Arithmetic,
                    &[
                        arith as u32,
                        dst,
                        left,
                        right,
                        call_constant,
                        function,
                        reuse,
                    ],
                );
                self.release_to(mark);
            }
        }
        Ok(())
    }

    unsafe fn for_loop(&mut self, call: Sexp, args: &[(Sexp, Sexp)], dst: u32) -> R<()> {
        let call_constant = self.constant(call);
        let variable = self.constant(args[0].1);
        let sequence = self.allocate();
        self.expression(args[1].1, sequence)?;
        let loop_index = self.open_loop();
        self.emit(Op::ForInit, &[loop_index as u32, sequence, call_constant]);
        let step = self.here();
        self.loops[loop_index].step = step;
        let exit = self.emit_patchable(Op::ForStep, &[loop_index as u32, sequence, variable, 0]);
        let mark = self.next_register;
        let body = self.allocate();
        self.expression(args[2].1, body)?;
        self.release_to(mark);
        self.back_edge(step);
        self.patch(exit);
        self.close_loop(loop_index);
        self.emit(Op::ForEnd, &[sequence]);
        self.release_to(sequence);
        self.loop_value(dst);
        Ok(())
    }

    fn open_loop(&mut self) -> usize {
        let start = self.here();
        self.loops.push(Loop {
            start,
            end: 0,
            exit: 0,
            step: 0,
        });
        self.open_loops
            .push((self.loops.len() - 1, self.cold.len()));
        self.loops.len() - 1
    }

    /// The backward jump that closes a loop's body, followed by the guard fallbacks compiled
    /// inside the body. They sit behind the unconditional jump, so the body runs straight
    /// through, and inside the loop's range, so a `break` or `next` the fallback's evaluator
    /// raises still finds this loop.
    fn back_edge(&mut self, top: u32) {
        self.emit(Op::Jump, &[top]);
        let mark = self.open_loops.last().map_or(0, |&(_, mark)| mark);
        let cold: SmallVec<[Cold; 4]> = self.cold.drain(mark..).collect();
        for cold in cold {
            for guard in cold.guards {
                self.patch(guard);
            }
            self.code.extend_from_slice(&cold.words);
            self.emit(Op::Resume, &[cold.resume]);
        }
    }

    /// Ends the loop at the current position: its exit is here, and every `break` and `next`
    /// compiled inside it now gets its target.
    fn close_loop(&mut self, loop_index: usize) {
        let exit = self.here();
        let entry = &mut self.loops[loop_index];
        entry.end = exit;
        entry.exit = exit;
        let (start, step) = (entry.start as usize, entry.step);
        let mut position = start;
        while position < exit as usize {
            let Some(op) = op_of(self.code[position]) else {
                panic!("the compiler emitted an invalid code word at {position}");
            };
            let width = operand_count(op);
            if op == Op::Jump {
                let operand = self.code[position + 1];
                if operand & MARKER_MASK != 0 && (operand & !MARKER_MASK) as usize == loop_index {
                    self.code[position + 1] = if operand & BREAK_MARKER != 0 {
                        exit
                    } else {
                        step
                    };
                }
            }
            position += 1 + width;
        }
        self.open_loops.pop();
    }

    fn loop_value(&mut self, dst: u32) {
        let constant = self.constant(nil());
        self.emit(Op::Constant, &[dst, constant]);
        self.emit(Op::Invisible, &[]);
    }

    /// Whether a call of `primitive` with `args` can evaluate its subject first, which is what a
    /// dispatch-or-eval special such as `[` does.
    unsafe fn eager_subject(&self, primitive: Sexp, args: &[(Sexp, Sexp)]) -> R<bool> {
        if primitive.kind() != SexpType::Special
            || !eval::builtin_def(primitive)?.is_dispatch_or_eval()
        {
            return Ok(false);
        }
        let Some(((_, subject), rest)) = args.split_first() else {
            return Ok(false);
        };
        let plain_argument = |value: Sexp| {
            value != symbols().dots
                && !may_modify_in_place(value)
                && (value.kind() != SexpType::Symbol
                    || value == missing_arg()
                    || eval::dot_dot_index(value).is_none())
        };
        Ok(*subject != missing_arg()
            && *subject != symbols().dots
            && rest.iter().all(|(_, value)| plain_argument(*value)))
    }

    /// A dispatch-or-eval special call: the subject evaluates first, an object dispatches with
    /// the other arguments as promises, and any other subject calls the special on values, the
    /// path the optimizing tier compiles.
    ///
    /// A formal among the other arguments may be missing, deeply so when it holds the caller's
    /// promise of a missing argument, and the special then passes on an empty argument, which a
    /// register cannot hold. Such a call evaluates only its subject here and leaves the rest to
    /// [`Op::DispatchSubject`] for every subject, which reads the formal's slot.
    unsafe fn subject_call(
        &mut self,
        call: Sexp,
        head: Sexp,
        primitive: Sexp,
        dst: u32,
        args: &[(Sexp, Sexp)],
        item: bool,
    ) -> R<()> {
        let symbol = self.constant(head);
        let expected = self.constant(primitive);
        let call_constant = self.call_site(call)?;
        let cache = self.cache();
        let mark = self.next_register;
        let function = self.allocate();
        self.emit(Op::GetFun, &[function, symbol, cache, call_constant]);
        let generic = self.emit_patchable(Op::BranchUnlessSame, &[function, expected, 0]);
        let base = self.next_register;
        for _ in args {
            self.allocate();
        }
        self.expression(args[0].1, base)?;
        // A record or tuple item with a constant index reads its slot through the site's shape
        // cache; anything else falls to `[[`'s path below.
        let read = if item {
            let index = self.constant(args[1].1);
            let shape = self.shape()?;
            let other = self.emit_patchable(Op::Item, &[dst, base, index, shape, 0]);
            let read = self.emit_patchable(Op::Jump, &[0]);
            self.patch(other);
            Some(read)
        } else {
            None
        };
        let formal = args[1..]
            .iter()
            .any(|(_, value)| self.formals.contains(value));
        if formal {
            self.emit(Op::DispatchSubject, &[dst, function, call_constant, base]);
            let dispatched = self.emit_patchable(Op::Jump, &[0]);
            self.patch(generic);
            self.emit(Op::CallGeneric, &[dst, function, call_constant]);
            self.patch(dispatched);
            if let Some(read) = read {
                self.patch(read);
            }
            self.release_to(mark);
            return Ok(());
        }
        let object = self.emit_patchable(Op::BranchIfObject, &[base, 0]);
        for (index, (_, value)) in args.iter().enumerate().skip(1) {
            if *value == missing_arg() {
                let constant = self.constant(missing_arg());
                self.emit(Op::Constant, &[base + index as u32, constant]);
            } else {
                self.expression(*value, base + index as u32)?;
            }
        }
        self.emit(
            Op::CallBuiltin,
            &[dst, function, call_constant, base, args.len() as u32],
        );
        let called = self.emit_patchable(Op::Jump, &[0]);
        self.patch(object);
        self.emit(Op::DispatchSubject, &[dst, function, call_constant, base]);
        let dispatched = self.emit_patchable(Op::Jump, &[0]);
        self.patch(generic);
        self.emit(Op::CallGeneric, &[dst, function, call_constant]);
        self.patch(called);
        self.patch(dispatched);
        if let Some(read) = read {
            self.patch(read);
        }
        self.release_to(mark);
        Ok(())
    }

    /// A fresh shape cache for a [`Op::Field`] or [`Op::Item`] site.
    unsafe fn shape(&mut self) -> R<u32> {
        let shape = values::alloc(SexpType::Integer, 2)?;
        values::ints_mut(shape).copy_from_slice(&[crate::records::NO_LAYOUT, 0]);
        Ok(self.constant(shape))
    }

    /// A record, tuple or tag constructor whose syntax fixes the layout: the items evaluate in
    /// order into the registers of their slots, and one instruction builds the value. A
    /// payload-free tag or the empty tuple is one shared constant.
    #[allow(clippy::too_many_arguments)]
    unsafe fn construct(
        &mut self,
        call: Sexp,
        head: Sexp,
        primitive: Sexp,
        dst: u32,
        items: &[(Sexp, Sexp)],
        layout: u32,
        slots: &[usize],
    ) -> R<()> {
        let guard = self.guard(call, head, primitive)?;
        if items.is_empty() {
            let value = crate::records::construct(layout, &[])?;
            let constant = self.constant(value);
            self.emit(Op::Constant, &[dst, constant]);
        } else {
            let mark = self.next_register;
            let base = self.next_register;
            for _ in items {
                self.allocate();
            }
            for ((_, value), slot) in items.iter().zip(slots) {
                self.expression(*value, base + *slot as u32)?;
            }
            self.emit(Op::Construct, &[dst, layout, base, items.len() as u32]);
            self.release_to(mark);
        }
        self.cold_fallback(guard.as_slice(), call, dst)?;
        Ok(())
    }

    /// `match` whose arms [`supported_match`] accepts: the scrutinee evaluates once, each arm's
    /// pattern compiles to tests that jump to the next arm, the names it binds are assigned once
    /// the whole pattern matched (so a failed arm binds nothing, as in the special), and the body
    /// compiles in place.
    unsafe fn compiled_match(
        &mut self,
        call: Sexp,
        head: Sexp,
        primitive: Sexp,
        dst: u32,
        args: &[(Sexp, Sexp)],
    ) -> R<()> {
        let guard = self.guard(call, head, primitive)?;
        let call_constant = self.constant(call);
        let mark = self.next_register;
        let value = self.allocate();
        self.expression(args[0].1, value)?;
        let mut matched = Vec::new();
        for (_, arm) in &args[1..] {
            let parts = values::pairlist_items(arm.cdr());
            let arm_mark = self.next_register;
            let mut failed = Vec::new();
            let mut bound = Vec::new();
            self.pattern(parts[0].1, value, &mut failed, &mut bound)?;
            if let [_, (_, guard), _] = parts.as_slice() {
                // The guard sees the bindings as the special's scratch frame would, so a guard
                // that fails leaves the function's own frame untouched. A guard of plain calls
                // reads them from registers; anything that could reach the frame by name runs
                // in a scratch frame, through `Op::BoundEval`.
                let names: Vec<Sexp> = bound.iter().map(|&(name, _)| name).collect();
                let list = self.constant(values::list_vector(&names)?);
                let base = self.next_register;
                for &(_, register) in &bound {
                    let copy = self.allocate();
                    self.emit(Op::Move, &[copy, register]);
                }
                let guard_constant = self.constant(*guard);
                let value = self.allocate();
                let in_place = guard_compiles_in_place(*guard, &names);
                self.bound = Some(Bound { names, list, base });
                if in_place {
                    self.expression(*guard, value)?;
                } else {
                    self.evaluate(value, guard_constant);
                }
                self.bound = None;
                let holds = self.allocate();
                self.emit(
                    Op::MatchCondition,
                    &[holds, value, guard_constant, call_constant],
                );
                failed.push(self.emit_patchable(Op::JumpUnless, &[holds, call_constant, 0]));
            }
            let body = parts.last().map_or(nil(), |&(_, body)| body);
            for (name, register) in bound {
                let name = self.constant(name);
                let slot = self.cache();
                self.emit(Op::SetVar, &[name, register, slot]);
            }
            self.expression(body, dst)?;
            matched.push(self.emit_patchable(Op::Jump, &[0]));
            for position in failed {
                self.patch(position);
            }
            self.release_to(arm_mark);
        }
        self.emit(Op::MatchFail, &[value, call_constant]);
        for position in matched {
            self.patch(position);
        }
        self.release_to(mark);
        self.cold_fallback(guard.as_slice(), call, dst)?;
        Ok(())
    }

    /// The tests of one pattern against the value in register `value`: each failing test's
    /// target operand goes to `failed`, and each name it binds, with the register holding the
    /// value, to `bound`.
    unsafe fn pattern(
        &mut self,
        pattern: Sexp,
        value: u32,
        failed: &mut Vec<usize>,
        bound: &mut Vec<(Sexp, u32)>,
    ) -> R<()> {
        match pattern.kind() {
            SexpType::Symbol => {
                if pattern.car().bytes() != b"_" {
                    bound.push((pattern, value));
                }
            }
            SexpType::Language => {
                let items = values::pairlist_items(pattern.cdr());
                match pattern.car().car().bytes() {
                    b"(" => self.pattern(items[0].1, value, failed, bound)?,
                    b"|" => {
                        let mut left = Vec::new();
                        self.pattern(items[0].1, value, &mut left, bound)?;
                        let either = self.emit_patchable(Op::Jump, &[0]);
                        for position in left {
                            self.patch(position);
                        }
                        self.pattern(items[1].1, value, failed, bound)?;
                        self.patch(either);
                    }
                    _ => match pattern_shape(pattern) {
                        Some(PatternShape::Tag(tag)) => {
                            failed.push(self.emit_patchable(Op::MatchTag, &[value, tag, 0]));
                        }
                        Some(PatternShape::Layout { layout, items }) => {
                            failed.push(self.emit_patchable(Op::MatchShape, &[value, layout, 0]));
                            for (item, slot) in items {
                                if item.kind() == SexpType::Symbol && item.car().bytes() == b"_" {
                                    continue;
                                }
                                let register = self.allocate();
                                self.emit(Op::Slot, &[register, value, slot as u32, layout]);
                                self.pattern(item, register, failed, bound)?;
                            }
                        }
                        None => panic!("a compiled `match` reached a pattern it does not support"),
                    },
                }
            }
            _ => {
                let constant = self.constant(pattern);
                failed.push(self.emit_patchable(Op::MatchLiteral, &[value, constant, 0]));
            }
        }
        Ok(())
    }

    /// `subject$name` for the base `$`, which evaluates its subject as a value, as the special
    /// does, and never evaluates the name.
    unsafe fn field(
        &mut self,
        call: Sexp,
        head: Sexp,
        primitive: Sexp,
        dst: u32,
        subject: Sexp,
        name: Sexp,
    ) -> R<()> {
        let symbol = self.constant(head);
        let expected = self.constant(primitive);
        let call_constant = self.call_site(call)?;
        let name_constant = self.constant(name);
        let shape = self.shape()?;
        let cache = self.cache();
        let mark = self.next_register;
        let function = self.allocate();
        self.emit(Op::GetFun, &[function, symbol, cache, call_constant]);
        let generic = self.emit_patchable(Op::BranchUnlessSame, &[function, expected, 0]);
        let object = self.allocate();
        self.expression(subject, object)?;
        self.emit(
            Op::Field,
            &[dst, object, name_constant, call_constant, shape],
        );
        let done = self.emit_patchable(Op::Jump, &[0]);
        self.patch(generic);
        self.generic_call(dst, function, call_constant, None);
        self.patch(done);
        self.release_to(mark);
        Ok(())
    }

    /// The constant [`Op::Closure`] makes a nested function's closures from: the body, or in
    /// code that binds its base functions, the body compiled now against this function's
    /// compile-time frames, as GNU R's compiler compiles a function's nested functions with it.
    unsafe fn nested_body(&mut self, formals: Sexp, body: Sexp, srcref: Sexp) -> R<u32> {
        self.force_binding()?;
        let Some(frames) = &self.binding else {
            return Ok(self.constant(body));
        };
        if !worth_compiling(body) {
            return Ok(self.constant(body));
        }
        let heap = crate::heap();
        let closure = eval::make_closure(formals, body, srcref, self.enclosure)?;
        heap.root(closure);
        let mut nested = Vec::with_capacity(frames.len() + 1);
        nested.push(Rc::new(compiler_locals(formals, body, self.enclosure)?));
        nested.extend(frames.iter().cloned());
        let code = compile_with_locals(body, closure, local_symbols(body, formals), Some(nested))?;
        heap.root(code);
        Ok(self.constant(code))
    }

    unsafe fn general_call(&mut self, call: Sexp, head: Sexp, dst: u32) -> R<()> {
        let rebinding = match self.rebinding {
            Some((value, target)) if value == call => {
                self.rebinding = None;
                Some(self.constant(target))
            }
            _ => None,
        };
        let symbol = self.constant(head);
        let call_constant = self.call_site(call)?;
        let cache = self.cache();
        let mark = self.next_register;
        let function = self.allocate();
        // A fused expression already checked that this name finds `expected`, and looking it up
        // again could find another function after a hoisted operand ran, as R's own lookup,
        // which happens before the arguments are evaluated, would not.
        match self
            .preguarded
            .iter()
            .find(|(operation, _)| *operation == call)
        {
            Some(&(_, expected)) => {
                let expected = self.constant(expected);
                self.emit(Op::Constant, &[function, expected]);
            }
            None => match self.bound_base(call, head)? {
                Some(bound) => {
                    if let Some(sites) = &mut self.sites {
                        sites.push(UNGUARDED);
                    }
                    let bound = self.constant(bound);
                    self.emit(Op::Constant, &[function, bound]);
                }
                None => self.emit(Op::GetFun, &[function, symbol, cache, call_constant]),
            },
        }
        let args = values::pairlist_items(call.cdr());
        // A register holds an argument's value without counting as a reference, so a later
        // argument that modifies a variable in place would change an earlier argument's value.
        // A record update that gives a field a call on the field's own value must not evaluate
        // that call before `eval::rebind_update` has taken the value out of the record.
        let updates_field = rebinding.is_some()
            && symbol_name(head).as_ref() == ".ry_update"
            && args
                .iter()
                .skip(1)
                .any(|&(name, value)| eval::updates_own_field(value, name, call.cdr().car()));
        let simple = !updates_field
            && args.iter().all(|(_, value)| *value != symbols().dots)
            && !args
                .iter()
                .skip(1)
                .any(|(_, value)| may_modify_in_place(*value));
        if simple {
            let generic = self.emit_patchable(Op::BranchUnlessBuiltin, &[function, 0]);
            let base = self.next_register;
            for _ in &args {
                self.allocate();
            }
            for (index, (_, value)) in args.iter().enumerate() {
                if *value == missing_arg() {
                    let constant = self.constant(missing_arg());
                    self.emit(Op::Constant, &[base + index as u32, constant]);
                } else {
                    self.expression(*value, base + index as u32)?;
                }
            }
            match rebinding {
                Some(target)
                    if crate::standard_library::reuses_first_argument(&symbol_name(head)) =>
                {
                    self.emit(
                        Op::CallBuiltinRebinding,
                        &[
                            dst,
                            function,
                            call_constant,
                            base,
                            args.len() as u32,
                            target,
                        ],
                    )
                }
                _ => self.emit(
                    Op::CallBuiltin,
                    &[dst, function, call_constant, base, args.len() as u32],
                ),
            };
            let done = self.emit_patchable(Op::Jump, &[0]);
            self.patch(generic);
            self.generic_call(dst, function, call_constant, rebinding);
            self.patch(done);
        } else {
            self.generic_call(dst, function, call_constant, rebinding);
        }
        self.release_to(mark);
        Ok(())
    }

    fn generic_call(&mut self, dst: u32, function: u32, call: u32, rebinding: Option<u32>) {
        // A closure's promises would read the frame, which does not hold a guard's bound names.
        if self.bound.is_some() {
            return self.evaluate(dst, call);
        }
        match rebinding {
            Some(target) => self.emit(Op::CallRebinding, &[dst, function, call, target]),
            None => self.emit(Op::CallGeneric, &[dst, function, call]),
        };
    }
}

const BREAK_MARKER: u32 = 1 << 30;
const NEXT_MARKER: u32 = 1 << 29;
const MARKER_MASK: u32 = BREAK_MARKER | NEXT_MARKER;

pub const fn operand_count(op: Op) -> usize {
    match op {
        Op::Constant => 2,
        Op::GetVar => 3,
        Op::GetFun => 4,
        Op::SetVar => 3,
        Op::BranchUnlessBuiltin => 2,
        Op::CallBuiltin => 5,
        Op::CallBuiltinRebinding => 6,
        Op::CallGeneric => 3,
        Op::CallRebinding | Op::CallTailUpdate => 4,
        Op::Field | Op::Item => 5,
        Op::MatchShape | Op::MatchTag | Op::MatchLiteral => 3,
        Op::Slot | Op::Construct => 4,
        Op::MatchFail | Op::Move => 2,
        Op::BoundEval => 5,
        Op::MatchCondition => 4,
        Op::SwitchArm => 3,
        Op::Ast => 2,
        Op::Guard => 5,
        Op::Jump | Op::Resume => 1,
        Op::Missing | Op::SuperAssign => 2,
        Op::Namespace => 5,
        Op::JumpUnless | Op::Prologue => 3,
        Op::Arithmetic => 7,
        Op::Return => 1,
        Op::ForInit => 3,
        Op::ForStep => 4,
        Op::ForEnd => 1,
        Op::Invisible | Op::Visible => 0,
        Op::LogicLeft => 5,
        Op::LogicRight => 5,
        Op::Closure => 4,
        Op::SubAssign => 7,
        Op::SubAssignMatrix => 6,
        Op::Replace => 8,
        Op::SetSrcref => 2,
        Op::EndStatement => 3,
        Op::BranchUnlessSame => 3,
        Op::BranchIfObject => 2,
        Op::DispatchSubject => 4,
        Op::Fused | Op::Unfused => 3,
        Op::Internal => 5,
    }
}

/// [`operand_count`] by opcode word, so dispatch reads the width with one load beside the
/// opcode instead of branching on the decoded op.
/// An instruction's first code word: the opcode in the low byte and the operand count above
/// it, so the interpreter decodes both without a table load.
pub const fn op_word(op: Op) -> u32 {
    op as u32 | (operand_count(op) as u32) << OP_BITS
}

/// The opcode of the code word `word`, unless it is not a valid one. A specialized form (see
/// [`QUICK_ARITHMETIC`]) reads as the instruction it specializes, so every reader other than
/// tier 0's dispatch sees the instructions the compiler emitted.
pub fn op_of(word: u32) -> Option<Op> {
    let index = (word & OP_MASK) as usize;
    let op = match OPS.get(index) {
        Some(&op) => op,
        None if index == POSITIONAL_CALL_BUILTIN => Op::CallBuiltin,
        None if index < HANDLER_COUNT => Op::Arithmetic,
        None => return None,
    };
    (op_word(op) & !OP_MASK == word & !(RESPECIALIZE_MASK | OP_MASK)).then_some(op)
}

const OP_BITS: u32 = 8;
const OP_MASK: u32 = (1 << OP_BITS) - 1;
/// The top byte of a generic instruction's word counts how often tier 0 specialized it and
/// had to undo the specialization; at [`RESPECIALIZE_LIMIT`] the site stays generic.
const RESPECIALIZE_SHIFT: u32 = 24;
const RESPECIALIZE_MASK: u32 = 0xFF << RESPECIALIZE_SHIFT;
const RESPECIALIZE_LIMIT: u32 = 4;

/// The base closures whose `.Internal` the compiler inlines, because the internal evaluates its
/// arguments, looks neither at the caller's frame nor at how it was called, and leaves the
/// result visible. The first twenty are GNU R's compiler's "safe base internals"; the rest are
/// wrappers that profiles of package code call often and that pass the same test: the
/// `vector` family, the string builders that forward `...`, and environment and row-name
/// queries.
const INLINED_BASE_INTERNALS: [&str; 34] = [
    "atan2",
    "besselY",
    "beta",
    "choose",
    "drop",
    "inherits",
    "is.vector",
    "lbeta",
    "lchoose",
    "nchar",
    "polyroot",
    "typeof",
    "vector",
    "which.max",
    "which.min",
    "is.loaded",
    "identical",
    "match",
    "rep.int",
    "rep_len",
    "character",
    "logical",
    "integer",
    "numeric",
    "double",
    "raw",
    "paste",
    "paste0",
    "sprintf",
    ".row_names_info",
    "make.unique",
    "startsWith",
    "endsWith",
    "isNamespace",
];

struct InlinedInternal {
    function: Sexp,
    internal: Sexp,
    arguments: SmallVec<[InternalArgument; 6]>,
    /// The call's arguments the closure's `...` takes, with their tags.
    dots: SmallVec<[(Sexp, Sexp); 6]>,
}

/// One argument of an inlined internal.
enum InternalArgument {
    /// The value of an expression evaluated in the caller.
    Value(Sexp),
    /// `list(...)` of the arguments the closure's `...` takes.
    List,
}

/// Tier 0's specialized forms of [`Op::Arithmetic`], by opcode minus `OPS.len()`: the operator
/// and whether the form takes doubles (one operand a double, the other a double or an integer)
/// rather than two integers. Tier 0 writes a form's opcode over a site's once the site computed
/// a result on two plain scalars of those kinds (quickening, as in CPython's PEP 659). A form
/// has the operands of the generic instruction, which [`op_of`] reports in its place, so the
/// optimizing tiers, the verifier and code images never see it. Its handler checks the operand
/// kinds with a masked header load each, and when they differ writes the generic opcode back.
const QUICK_ARITHMETIC: [(Arith, bool); 19] = [
    (Arith::Add, false),
    (Arith::Subtract, false),
    (Arith::Multiply, false),
    (Arith::Equal, false),
    (Arith::NotEqual, false),
    (Arith::Less, false),
    (Arith::Greater, false),
    (Arith::LessEqual, false),
    (Arith::GreaterEqual, false),
    (Arith::Add, true),
    (Arith::Subtract, true),
    (Arith::Multiply, true),
    (Arith::Divide, true),
    (Arith::Equal, true),
    (Arith::NotEqual, true),
    (Arith::Less, true),
    (Arith::Greater, true),
    (Arith::LessEqual, true),
    (Arith::GreaterEqual, true),
];

/// Tier 0's form of [`Op::CallBuiltin`] for a site whose argument cells are all untagged and
/// none empty ([`positional_call`]), which is a fact of the call. The compiler emits it, so no
/// code page is written at run time, which keeps the start-up image's privately mapped code
/// shared between processes. Its handler fills the arguments straight from the registers,
/// skipping the walk over the call's cells. Like the arithmetic forms, [`op_of`] reports it as
/// the generic instruction.
const POSITIONAL_CALL_BUILTIN: usize = OPS.len() + QUICK_ARITHMETIC.len();

/// The code word of [`POSITIONAL_CALL_BUILTIN`].
const POSITIONAL_CALL_BUILTIN_WORD: u32 =
    (op_word(Op::CallBuiltin) & !OP_MASK) | POSITIONAL_CALL_BUILTIN as u32;

/// The size of tier 0's handler tables: every generic and specialized opcode.
const HANDLER_COUNT: usize = POSITIONAL_CALL_BUILTIN + 1;

// The interpreter turns a code word's low byte back into an `Op` by its discriminant.
const _: () = {
    let mut index = 0;
    while index < OPS.len() {
        assert!(OPS[index] as usize == index && operand_count(OPS[index]) < 1 << 16);
        index += 1;
    }
    assert!(HANDLER_COUNT <= OP_MASK as usize + 1);
};

/// What an operand of an instruction is, as far as [`verify`] checks it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Operand {
    Register,
    /// The first of a run of registers, whose length the next operand gives.
    Base,
    /// The length of the run of registers the previous operand starts.
    Count,
    /// [`Operand::Count`] for a run that holds at least one register.
    CountAtLeastOne,
    /// Any constant.
    Constant,
    Symbol,
    /// A call, which the handler takes apart.
    Call,
    /// A call followed by the constants [`Compiler::call_site`] puts after it.
    CallSite,
    /// A pairlist of formals, or `NULL`.
    Formals,
    /// A [`Op::Field`] or [`Op::Item`] site's shape cache, still empty.
    Shape,
    /// The constant index of an [`Op::Item`]: one string or number.
    Index,
    /// A list of symbols, at least as long as the run of registers after it.
    Names,
    /// A [`switch_table`].
    Switch,
    Cache,
    Target,
    Loop,
    /// An operator of [`ARITHS`].
    Operator,
    /// A number the handler reads without indexing anything with it.
    Immediate,
    /// A fused program ([`fusion`]), checked by [`valid_fusion_program`].
    Fusion,
}

/// The operands of `op`, or `None` for an instruction a code image never holds: a record
/// instruction names a record layout by the number this process interned it under, which
/// means nothing in another process.
fn operands(op: Op) -> Option<&'static [Operand]> {
    use Operand::*;
    Some(match op {
        Op::Constant | Op::Ast => &[Register, Constant],
        Op::GetVar => &[Register, Symbol, Cache],
        Op::SetVar => &[Symbol, Register, Cache],
        Op::GetFun => &[Register, Symbol, Cache, Call],
        Op::BranchUnlessBuiltin | Op::BranchIfObject => &[Register, Target],
        Op::CallBuiltin => &[Register, Register, Call, Base, Count],
        Op::CallBuiltinRebinding => &[Register, Register, Call, Base, Count, Symbol],
        Op::CallGeneric => &[Register, Register, CallSite],
        Op::CallRebinding | Op::CallTailUpdate => &[Register, Register, Call, Symbol],
        Op::Field => &[Register, Register, Symbol, Call, Shape],
        Op::Item => &[Register, Register, Index, Shape, Target],
        Op::MatchLiteral => &[Register, Constant, Target],
        Op::MatchFail => &[Register, Constant],
        Op::Move => &[Register, Register],
        Op::BoundEval => &[Register, Constant, Names, Base, Count],
        Op::MatchCondition => &[Register, Register, Constant, Constant],
        Op::SwitchArm => &[Register, Register, Switch],
        Op::Guard => &[Symbol, Constant, Cache, Immediate, Target],
        Op::Jump | Op::Resume => &[Target],
        Op::JumpUnless => &[Register, Constant, Target],
        Op::Arithmetic => &[Operator, Register, Register, Register, Call, Constant],
        Op::Return | Op::ForEnd => &[Register],
        Op::Missing => &[Register, Symbol],
        Op::Namespace => &[Register, Symbol, Symbol, Immediate, Cache],
        Op::SuperAssign => &[Symbol, Register],
        Op::Prologue => &[Register, Constant, Call],
        Op::ForInit => &[Loop, Register, Constant],
        Op::ForStep => &[Loop, Register, Symbol, Target],
        Op::Invisible | Op::Visible => &[],
        Op::LogicLeft => &[Register, Register, Constant, Immediate, Target],
        Op::LogicRight => &[Register, Register, Register, Constant, Immediate],
        Op::Closure => &[Register, Formals, Constant, Constant],
        Op::SubAssign => &[Register, Symbol, Register, Register, Call, Immediate],
        Op::SubAssignMatrix => &[Register, Symbol, Register, Register, Register, Call],
        Op::Replace => &[
            Register,
            Symbol,
            Symbol,
            Cache,
            Call,
            Base,
            CountAtLeastOne,
            Call,
        ],
        Op::SetSrcref => &[Constant, Immediate],
        Op::EndStatement => &[Register, Base, Base],
        Op::BranchUnlessSame => &[Register, Constant, Target],
        Op::DispatchSubject => &[Register, Register, Call, Register],
        Op::Fused | Op::Unfused => &[Register, Fusion, Target],
        Op::Internal => &[Register, Call, Constant, Base, Count],
        Op::MatchShape | Op::MatchTag | Op::Slot | Op::Construct => return None,
    })
}

/// Checks that `code`, a code object read from a file rather than made by the compiler, is one
/// the interpreter can run without reading or writing outside its tables. The interpreter reads
/// operands unchecked, so a code image's code must pass this before it runs: every word is a
/// valid instruction or one of its operands, execution cannot run off the end, every register,
/// constant, cache slot, loop and jump target an operand names exists, and every constant a
/// handler takes apart has the type it expects. Code objects among the constants are checked
/// where they are decoded.
///
/// # Safety
/// `code` is a live object.
pub unsafe fn verify(code: Sexp) -> Result<(), String> {
    use Operand::*;
    if code.kind() != SexpType::Bytecode
        || code.car().kind() != SexpType::Integer
        || code.cdr().kind() != SexpType::List
        || code.tag().kind() != SexpType::List
    {
        return Err("not a code object".to_owned());
    }
    let words = values::ints(code.car());
    let constants = code.cdr();
    let constant_count = length(constants);
    if constant_count < FIRST_CONSTANT {
        return Err("a code object without its fixed constants".to_owned());
    }
    let table = |slot: usize, kind: SexpType, length_is: Option<usize>| {
        let value = constants.element(slot);
        value.kind() == kind && length_is.is_none_or(|expected| length(value) == expected)
    };
    let epochs = constants.element(EPOCHS);
    if epochs.kind() != SexpType::Real {
        return Err("a malformed inline cache table".to_owned());
    }
    let caches = length(epochs);
    let registers = constants.element(REGISTERS);
    if !table(HINTS, SexpType::Integer, Some(caches))
        || length(code.tag()) != caches * 2
        || !table(FEEDBACK, SexpType::Integer, Some(words.len()))
        || !table(PROFILE, SexpType::Integer, Some(profile::LENGTH))
        || !table(LOOPS, SexpType::Integer, None)
        || length(constants.element(LOOPS)) % 4 != 0
        || !table(REGISTERS, SexpType::Integer, Some(1))
        || values::ints(registers)[0] < 0
        || constants.element(TIER1) != nil()
        || constants.element(MATCHES) != nil()
    {
        return Err("a code object whose tables disagree".to_owned());
    }
    let registers = values::ints(registers)[0] as usize;
    let summary = constants.element(SUMMARY);
    if summary != nil()
        && (summary.kind() != SexpType::List
            || length(summary) == 0
            || summary.element(0).kind() != SexpType::Integer
            || length(summary.element(0)) != 1
            || !(0..caches as i64).contains(&i64::from(values::ints(summary.element(0))[0]))
            || (1..length(summary)).any(|index| summary.element(index).kind() != SexpType::Symbol))
    {
        return Err("a malformed effect summary".to_owned());
    }
    let slots = constants.element(SLOTS);
    if slots != nil() {
        let well_formed = slots.kind() == SexpType::List
            && length(slots) == 4
            && slots.element(2) == nil()
            && (slots.element(3) == nil()
                || slots.element(3).kind() == SexpType::Integer
                    && values::ints(slots.element(3)).iter().all(|&slot| {
                        (-1..length(slots.element(1)) as i64).contains(&i64::from(slot))
                    }))
            && slots.element(0).kind() == SexpType::Integer
            && length(slots.element(0)) == caches
            && slots.element(1).kind() == SexpType::List
            && (0..length(slots.element(1)))
                .all(|index| slots.element(1).element(index).kind() == SexpType::Symbol)
            && values::ints(slots.element(0))
                .iter()
                .all(|&slot| (-1..length(slots.element(1)) as i64).contains(&i64::from(slot)));
        if !well_formed {
            return Err("a malformed slot table".to_owned());
        }
    }
    let mut starts = vec![0u64; words.len().div_ceil(64)];
    let mut pc = 0;
    while pc < words.len() {
        let op = op_of(words[pc] as u32).ok_or("an invalid instruction")?;
        // Specialized forms trust what tier 0 saw when it wrote them, which a file cannot show.
        // The positional builtin call is the exception: its call constant shows it, and the
        // operand pass below checks that.
        if words[pc] as u32 != op_word(op) && words[pc] as u32 != POSITIONAL_CALL_BUILTIN_WORD {
            return Err("a specialized instruction".to_owned());
        }
        operands(op).ok_or("a record instruction")?;
        starts[pc / 64] |= 1 << (pc % 64);
        let next = pc + 1 + operand_count(op);
        if next > words.len() {
            return Err("an instruction runs past the end of the code".to_owned());
        }
        if next == words.len() && !matches!(op, Op::Return | Op::Jump | Op::Resume) {
            return Err("execution runs past the end of the code".to_owned());
        }
        pc = next;
    }
    if words.is_empty() {
        return Err("empty code".to_owned());
    }
    let is_start =
        |target: usize| target < words.len() && starts[target / 64] & (1 << (target % 64)) != 0;
    let loop_table = values::ints(constants.element(LOOPS));
    for entry in loop_table.as_chunks::<4>().0 {
        let [start, end, exit, step] = entry.map(|word| word as usize);
        if start > end || end > words.len() || !is_start(exit) || !is_start(step) {
            return Err("a malformed loop table".to_owned());
        }
    }
    let loops = loop_table.len() / 4;
    let constant = |index: usize| {
        (FIRST_CONSTANT..constant_count)
            .contains(&index)
            .then(|| constants.element(index))
            .ok_or("a constant index out of range")
    };
    let mut pc = 0;
    while pc < words.len() {
        let op = op_of(words[pc] as u32).ok_or("an invalid instruction")?;
        let roles = operands(op).ok_or("a record instruction")?;
        let operand = |index: usize| words[pc + 1 + index] as u32 as usize;
        for (index, &role) in roles.iter().enumerate() {
            let value = operand(index);
            let valid = match role {
                Register => value < registers,
                Base => value <= registers,
                Count | CountAtLeastOne => {
                    value <= registers - operand(index - 1) && (role == Count || value >= 1)
                }
                Constant => constant(value).is_ok(),
                Symbol => constant(value)?.kind() == SexpType::Symbol,
                Call => constant(value)?.kind() == SexpType::Language,
                CallSite => {
                    let arguments = constant(value + 1)?;
                    let eager = constant(value + 2)?;
                    constant(value)?.kind() == SexpType::Language
                        && (arguments == unbound()
                            || matches!(arguments.kind(), SexpType::Nil | SexpType::Pairlist))
                        && (eager == nil() || eager.kind() == SexpType::Logical)
                }
                Formals => matches!(constant(value)?.kind(), SexpType::Nil | SexpType::Pairlist),
                Shape => {
                    let shape = constant(value)?;
                    shape.kind() == SexpType::Integer
                        && values::ints(shape) == [crate::records::NO_LAYOUT, 0]
                }
                Index => {
                    let index = constant(value)?;
                    matches!(
                        index.kind(),
                        SexpType::String | SexpType::Integer | SexpType::Real
                    ) && length(index) == 1
                }
                Names => {
                    let names = constant(value)?;
                    names.kind() == SexpType::List
                        && length(names) >= operand(index + 2)
                        && (0..length(names))
                            .all(|name| names.element(name).kind() == SexpType::Symbol)
                }
                Switch => valid_switch_table(constant(value)?, constant),
                Cache => value < caches,
                Target => is_start(value),
                Loop => value < loops,
                Operator => value < ARITHS.len(),
                Immediate => true,
                Fusion => valid_fusion_program(
                    constant(value)?,
                    &constant,
                    caches,
                    words.len(),
                    registers,
                ),
            };
            if !valid {
                return Err(format!("an invalid operand {index} of {op:?} at {pc}"));
            }
        }
        if op == Op::EndStatement && operand(1) > operand(2) {
            return Err(format!("an invalid register range at {pc}"));
        }
        if words[pc] as u32 == POSITIONAL_CALL_BUILTIN_WORD
            && !positional_call(constant(operand(2))?, operand(4))
        {
            return Err(format!(
                "a positional builtin call with other arguments at {pc}"
            ));
        }
        pc += 1 + operand_count(op);
    }
    Ok(())
}

/// Whether `program` is `NULL` or has the layout [`fusion`] describes, with every constant,
/// cache slot, feedback site and node input in range, which [`fused_pass`] reads unchecked.
/// Nodes are in evaluation order, so a node's inputs come before it and the root is last.
unsafe fn valid_fusion_program(
    program: Sexp,
    constant: &impl Fn(usize) -> Result<Sexp, &'static str>,
    caches: usize,
    sites: usize,
    registers: usize,
) -> bool {
    if program == nil() {
        return true;
    }
    if program.kind() != SexpType::Integer || length(program) < fusion::HEADER {
        return false;
    }
    let words = values::ints(program);
    let count = |index: usize| usize::try_from(words[index]).ok();
    let (Some(nodes), Some(leaves), Some(guards)) = (
        count(fusion::NODES),
        count(fusion::LEAVES),
        count(fusion::GUARDS),
    ) else {
        return false;
    };
    let guards_end = fusion::HEADER + guards * fusion::GUARD;
    let leaves_end = guards_end + leaves * fusion::LEAF;
    if nodes == 0
        || nodes > MAX_NODES
        || leaves > MAX_NODES
        || guards > MAX_NODES
        || words.len() != leaves_end + nodes * fusion::NODE
        || !(fusion::ASSIGNED..=2).contains(&words[fusion::REDUCTION])
        || count(fusion::REDUCTION_SITE).is_none_or(|site| site >= sites)
    {
        return false;
    }
    let below = |word: i32, limit: usize| usize::try_from(word).is_ok_and(|value| value < limit);
    let symbol = |word: i32| {
        usize::try_from(word)
            .ok()
            .and_then(|index| constant(index).ok())
            .is_some_and(|value| value.kind() == SexpType::Symbol)
    };
    let any_constant = |word: i32| {
        usize::try_from(word)
            .ok()
            .is_some_and(|index| constant(index).is_ok())
    };
    let guards_valid = words[fusion::HEADER..guards_end]
        .chunks_exact(fusion::GUARD)
        .all(|guard| symbol(guard[0]) && any_constant(guard[1]) && below(guard[2], caches));
    let leaves_valid = words[guards_end..leaves_end]
        .chunks_exact(fusion::LEAF)
        .all(|leaf| match leaf[0] {
            0 => any_constant(leaf[1]),
            1 => symbol(leaf[1]) && below(leaf[2], caches) && below(leaf[3], sites),
            2 => below(leaf[1], registers),
            _ => false,
        });
    let nodes_valid = words[leaves_end..]
        .chunks_exact(fusion::NODE)
        .enumerate()
        .all(|(index, node)| {
            let input = |word: i32| below(word, index);
            let site = below(node[4], sites);
            match node[0] {
                0 => below(node[1], leaves),
                1 => site && below(node[1], ARITHS.len()) && input(node[2]) && input(node[3]),
                2 => site && input(node[2]) && input(node[3]),
                3 | 4 => site && input(node[1]),
                5 => site && below(node[1], MATHS.len()) && input(node[2]),
                _ => false,
            }
        });
    guards_valid && leaves_valid && nodes_valid
}

/// Whether `table` has the shape [`switch_table`] gives it, which [`switch_arm`] reads unchecked.
unsafe fn valid_switch_table<'a>(
    table: Sexp,
    constant: impl Fn(usize) -> Result<Sexp, &'a str>,
) -> bool {
    if table.kind() != SexpType::List || length(table) != SWITCH_CALL + 1 {
        return false;
    }
    let names = table.element(SWITCH_NAMES);
    let name_arms = table.element(SWITCH_NAME_ARMS);
    let position_arms = table.element(SWITCH_POSITION_ARMS);
    let numbers = table.element(SWITCH_NUMBERS);
    if names.kind() != SexpType::String
        || name_arms.kind() != SexpType::Integer
        || length(name_arms) != length(names)
        || position_arms.kind() != SexpType::Integer
        || length(position_arms) == 0
        || numbers.kind() != SexpType::Integer
        || table.element(SWITCH_CALL).kind() != SexpType::Language
    {
        return false;
    }
    let arms = length(numbers) as i64;
    // A name's arm indexes the numbers directly, while a negative position arm is an error
    // `switch_arm` raises before indexing.
    values::ints(numbers).iter().all(|&index| {
        usize::try_from(index)
            .ok()
            .and_then(|index| constant(index).ok())
            .is_some_and(|number| number.kind() == SexpType::Integer && length(number) == 1)
    }) && values::ints(name_arms)
        .iter()
        .all(|&arm| (0..arms).contains(&i64::from(arm)))
        && values::ints(position_arms)
            .iter()
            .all(|&arm| (-1..arms).contains(&i64::from(arm)))
}

// The interpreter.

/// One `for` loop's position in its sequence.
#[derive(Clone, Copy, Default)]
pub struct LoopState {
    pub index: usize,
    pub count: usize,
    /// The first element of a compact sequence such as `1:n`, which the loop steps through
    /// without expanding it.
    pub first: f64,
    /// The compact sequence's increment, 1 or -1, or 0 when the loop reads its elements from
    /// the sequence's storage.
    pub step: f64,
    /// The next cell, for a pairlist sequence.
    cell: Option<Sexp>,
}

impl LoopState {
    /// The state of a loop over a vector or a compact sequence, which is all the optimizing
    /// tier compiles.
    pub fn vector(index: usize, count: usize, first: f64, step: f64) -> LoopState {
        LoopState {
            index,
            count,
            first,
            step,
            cell: None,
        }
    }
}

/// Stores the next element of an atomic `sequence` into the loop variable's current scalar
/// in place, as GNU R's `STEPFOR` does, instead of allocating a fresh scalar per iteration.
/// Only a scalar that nothing but the binding references may change: a count above one means
/// the body stored the value somewhere, and that copy must keep the old element.
/// `value` is what the loop variable's binding or slot holds, which counts as its one reference.
unsafe fn overwrite_loop_variable(sequence: Sexp, index: usize, value: Sexp) -> bool {
    if sequence.is_altrep() {
        return false;
    }
    match sequence.kind() {
        SexpType::Real => overwrite_scalar(value, SexpType::Real, sequence.slice::<f64>()[index]),
        kind @ (SexpType::Integer | SexpType::Logical) => {
            overwrite_scalar(value, kind, f64::from(sequence.slice::<i32>()[index]))
        }
        _ => false,
    }
}

/// Stores `element` into `value` when it is a plain scalar of `kind` that nothing but the loop
/// variable references. An integer or logical element is exact in a double.
unsafe fn overwrite_scalar(value: Sexp, kind: SexpType, element: f64) -> bool {
    if value.kind() != kind
        || value.is_altrep()
        || value.refcount() != 1
        || value.length() != 1
        || values::has_attributes(value)
    {
        return false;
    }
    match kind {
        SexpType::Real => value.slice_mut::<f64>()[0] = element,
        _ => value.slice_mut::<i32>()[0] = element as i32,
    }
    true
}

/// The number of `for` loops in `code`.
pub unsafe fn loop_count(code: Sexp) -> usize {
    pool_entry(code.cdr(), LOOPS).length() as usize / 4
}

/// Entry `index` of a code object's constant pool. The compiler builds the pool and the
/// vectors in its fixed entries itself, so none of them is ALTREP and the entry path reads them
/// without the element accessor's ALTREP test.
#[inline(always)]
unsafe fn pool_entry(constants: Sexp, index: usize) -> Sexp {
    debug_assert!(!constants.is_altrep());
    *constants.data().cast::<Sexp>().add(index)
}

/// The integer words of the pool's fixed entry `index`, such as [`PROFILE`] or [`REGISTERS`].
#[inline(always)]
unsafe fn pool_ints(constants: Sexp, index: usize) -> *mut i32 {
    let entry = pool_entry(constants, index);
    debug_assert!(!entry.is_altrep());
    entry.data().cast::<i32>()
}

/// Runs a compiled promise's code in `env`, the frame it was compiled for. Promise code has no
/// loops (`compiles_as_promise` rejects them) and runs for no closure, so this skips the entry
/// profile, the loop states and the optimizing tier that `run` sets up for a function body.
pub unsafe fn run_promise(code: Sexp, env: Sexp) -> R<Sexp> {
    let heap = crate::heap();
    heap.root(code);
    let mark = heap.handle_mark();
    let register_count = *pool_ints(code.cdr(), REGISTERS) as usize;
    let header = interp().bytecode.registers.len();
    let base = header + 1;
    reserve_registers(base + register_count)?;
    *register_address(header) = env;
    let was_compiled_frame = env.has_gp(gp::COMPILED_FRAME);
    env.set_gp_bits(gp::COMPILED_FRAME, true);
    let srcref = interp().srcref;
    let mut machine = Machine {
        function: nil(),
        code,
        header,
        base,
        mark,
        loops: &mut [],
    };
    let result = execute_code(&mut machine, 0);
    interp().srcref = srcref;
    interp().bytecode.registers.truncate(header);
    env.set_gp_bits(gp::COMPILED_FRAME, was_compiled_frame);
    match result {
        Err(Unwind::Error(failure)) => Err(conditions::signal_error(failure)),
        other => other,
    }
}

/// Runs a promise's `code`, compiled against the slot table of `function`'s body, on the slots
/// of the live slot frame at position `owner`, whose header register is `header`. The code may
/// give the frame its environment, after which it reads and writes that as the body does.
pub unsafe fn run_promise_on_slots(
    code: Sexp,
    function: Sexp,
    owner: usize,
    header: usize,
) -> R<Sexp> {
    let heap = crate::heap();
    heap.root(code);
    let mark = heap.handle_mark();
    let register_count = *pool_ints(code.cdr(), REGISTERS) as usize;
    let base = interp().bytecode.registers.len();
    reserve_registers(base + register_count)?;
    let state = interp();
    let previous_owner = state.slot_owner;
    state.slot_owner = owner as u32;
    let srcref = state.srcref;
    let mut machine = Machine {
        function,
        code,
        header,
        base,
        mark,
        loops: &mut [],
    };
    let result = execute_code(&mut machine, 0);
    let state = interp();
    state.srcref = srcref;
    state.slot_owner = previous_owner;
    state.bytecode.registers.truncate(base);
    match result {
        Err(Unwind::Error(failure)) => Err(conditions::signal_error(failure)),
        other => other,
    }
}

unsafe fn run(function: Sexp, code: Sexp, env: Sexp) -> R<Sexp> {
    let header = interp().bytecode.registers.len();
    reserve_registers(header + 1)?;
    *register_address(header) = env;
    let result = run_at(function, code, header, header + 1);
    interp().bytecode.registers.truncate(header);
    result
}

/// Runs the body of `function`, compiled to `code`, in the slot frame whose header register is
/// `header` and whose `slots` slots follow it. The caller truncates the register file to the header.
pub unsafe fn run_slots(function: Sexp, header: usize, slots: usize) -> R<Sexp> {
    run_at(function, function.cdr(), header, header + 1 + slots)
}

/// How many slots the frame of `code`, compiled with a slot table, has.
pub unsafe fn slot_count(code: Sexp) -> usize {
    length(code.cdr().element(SLOTS).element(1))
}

/// Runs `code` in the activation whose header register is `header` and whose own registers
/// start at `base`, after the header and any slots.
unsafe fn run_at(function: Sexp, code: Sexp, header: usize, base: usize) -> R<Sexp> {
    let heap = crate::heap();
    heap.root(code);
    let mark = heap.handle_mark();
    let constants = code.cdr();
    let register_count = *pool_ints(constants, REGISTERS) as usize;
    reserve_registers(base + register_count)?;
    let profile = pool_ints(constants, PROFILE);
    let entries = (*profile.add(profile::ENTRIES)).saturating_add(1);
    *profile.add(profile::ENTRIES) = entries;
    // Built empty and filled only for a body with loops, so the inline buffer is not assembled
    // on the stack and copied, which every call of a body paid.
    let mut loops: SmallVec<[LoopState; 4]> = SmallVec::new();
    let count = loop_count(code);
    if count != 0 {
        loops.extend(std::iter::repeat_n(LoopState::default(), count));
    }
    let env = *register_address(header);
    // A slot frame's environment, once it has one, is fresh and marked as compiled when made.
    let was_compiled_frame = env != nil() && env.has_gp(gp::COMPILED_FRAME);
    if env != nil() {
        env.set_gp_bits(gp::COMPILED_FRAME, true);
    }
    let mut machine = Machine {
        function,
        code,
        header,
        base,
        mark,
        loops: &mut loops,
    };
    // The tiers' schedule fires at a few entries of a body, so the common entry runs the code
    // here, in this frame, and the scheduled one goes out of line.
    let result = if entries >= *profile.add(profile::NEXT) {
        run_scheduled_entry(&mut machine, entries)
    } else {
        execute_code(&mut machine, 0)
    };
    interp().bytecode.registers.truncate(base);
    let env = *register_address(header);
    if env != nil() {
        env.set_gp_bits(gp::COMPILED_FRAME, was_compiled_frame);
    }
    match result {
        Err(Unwind::Error(failure)) => Err(conditions::signal_error(failure)),
        other => other,
    }
}

/// Runs an activation whose entry the tiers' schedule picked: the optimizing tier may run it,
/// or hand it back to resume at a pc.
#[inline(never)]
unsafe fn run_scheduled_entry(machine: &mut Machine<'_>, entries: i32) -> R<Sexp> {
    match entry_event(machine, entries) {
        Some(TierExit::Finished(result)) => result,
        Some(TierExit::Resume { pc, pending }) => match pending {
            Some(unwind) => {
                continue_after(machine, pc, unwind).and_then(|pc| execute_code(machine, pc))
            }
            None => execute_code(machine, pc),
        },
        None => execute_code(machine, 0),
    }
}

/// The entry count at which a body without optimized code starts recording type feedback
/// for a compile at `threshold` entries: the last eighth of the way, and at least the entry
/// before a compile at the second entry.
pub fn recording_start(threshold: i32) -> i32 {
    threshold - (threshold / 8).max(threshold.min(1))
}

/// What the tiers do at an entry of `machine`'s code numbered `entries`, the decisions
/// [`profile::NEXT`] schedules. Returns the optimizing tier's exit when it ran the activation.
#[inline(never)]
unsafe fn entry_event(machine: &mut Machine<'_>, entries: i32) -> Option<TierExit> {
    let constants = machine.code.cdr();
    let Some(tier) = tier() else {
        let profile = values::ints_mut(constants.element(PROFILE));
        profile[profile::RECORDING] = 0;
        // Code that ran without a tier asks again now and then, so a tier installed later still
        // reaches it, at one call in this many.
        profile[profile::NEXT] = entries.saturating_add(1 << 12);
        return None;
    };
    let profile = values::ints_mut(constants.element(PROFILE));
    let optimized = constants.element(TIER1) != nil();
    if !optimized {
        if entries == recording_start(tier.entry_threshold) && profile[profile::TIER_STATE] >= 0 {
            let worth = (tier.worth_recording)(machine);
            values::ints_mut(constants.element(PROFILE))[profile::RECORDING] = i32::from(worth);
        } else if entries == 2 && entries < recording_start(tier.entry_threshold) {
            profile[profile::RECORDING] = 0;
        }
    }
    // Most bodies never compile, and asking the tier about every call of every body cost more
    // than the compiled code saved on a test suite.
    let entered = if optimized
        || ((entries == 1 || entries >= tier.entry_threshold) && profile[profile::TIER_STATE] >= 0)
    {
        (tier.enter)(machine)
    } else {
        None
    };
    // The tier may have compiled, refused, or moved the counters, so the schedule is derived
    // from what they hold now.
    let profile = values::ints_mut(constants.element(PROFILE));
    let entries = profile[profile::ENTRIES];
    let state = profile[profile::TIER_STATE];
    profile[profile::NEXT] =
        if constants.element(TIER1) != nil() || (state >= 0 && entries >= tier.entry_threshold) {
            i32::MIN
        } else {
            let start = recording_start(tier.entry_threshold);
            [
                (state >= 0).then_some(1),
                (2 < start).then_some(2),
                (state >= 0).then_some(start),
                (state >= 0).then_some(tier.entry_threshold),
            ]
            .into_iter()
            .flatten()
            .filter(|&count| count > entries)
            .min()
            .unwrap_or(NEVER)
        };
    entered
}

#[inline(always)]
unsafe fn execute_code(machine: &mut Machine<'_>, pc: usize) -> R<Sexp> {
    let _activity = activity::enter(Activity::Interpreter, 0);
    interpret::<false>(machine, pc).0
}

/// Where interpretation continues after the instruction at `pc` unwound: the loop target of a
/// `break` or `next` aimed at this activation, or nowhere, when the unwind propagates.
unsafe fn continue_after(machine: &mut Machine<'_>, pc: usize, unwind: Unwind) -> R<usize> {
    let Unwind::Jump(jump) = unwind else {
        return Err(unwind);
    };
    let loops = machine.code.cdr().element(LOOPS);
    let target = match jump {
        Jump::Break { env: target } if target == machine.env() => loop_target(loops, pc, true),
        Jump::Next { env: target } if target == machine.env() => loop_target(loops, pc, false),
        _ => None,
    };
    match target {
        Some(target) => {
            crate::heap().release_handles(machine.mark);
            Ok(target)
        }
        None => Err(Unwind::Jump(jump)),
    }
}

/// Where a `break` (or `next`) that unwound out of the instruction at `pc` continues: the exit
/// (or step) of the innermost loop containing `pc`.
unsafe fn loop_target(loops: Sexp, pc: usize, is_break: bool) -> Option<usize> {
    let table = values::ints(loops);
    let mut best: Option<(usize, usize)> = None;
    for entry in table.as_chunks::<4>().0 {
        let (start, end) = (entry[0] as usize, entry[1] as usize);
        if start <= pc && pc < end && best.is_none_or(|(best_start, _)| start >= best_start) {
            best = Some((start, if is_break { entry[2] } else { entry[3] } as usize));
        }
    }
    best.map(|(_, target)| target)
}

/// Runs the one instruction at `pc` for the optimizing tier, which calls it for instructions it
/// does not specialize. It must not be a jump, a branch, `Return` or a loop instruction. Answers
/// the value the activation returns when the instruction returns from it, as a finished
/// [`Op::Prologue`] does.
pub unsafe fn step_one(machine: &mut Machine<'_>, pc: usize) -> R<Option<Sexp>> {
    match interpret::<true>(machine, pc) {
        (Ok(value), STOP) => Ok(Some(value)),
        (result, _) => result.map(|_| None),
    }
}

/// Runs the element read or store at `pc` through tier 0's fast handler for it, for the
/// optimizing tier, and says whether the handler applied: it then read and wrote only the
/// instruction's registers, recorded its feedback and, for `SubAssign`, wrote the element of
/// its variable's vector in place.
pub unsafe fn fast_step(machine: &mut Machine<'_>, pc: usize) -> bool {
    let mut state = activation(machine, true);
    let applied = match op_of(*state.code.add(pc) as u32) {
        Some(Op::CallBuiltin) => call_builtin_fast(&mut state, pc),
        Some(Op::SubAssign) => sub_assign_fast(&mut state, pc),
        Some(Op::Field) => field_fast(&mut state, pc),
        _ => None,
    };
    applied.is_some()
}

/// Runs the element read or store at `pc` through its generic handler when that runs no R code,
/// for the optimizing tier where [`fast_step`] declined, so that no frame needs writing back. It
/// may rebind `SubAssign`'s variable. `None` when the handler might run R code, and nothing
/// ran: a calling handler is established, which a warning would run, or an operand is an
/// object, an ALTREP vector, an environment or a promise, whose methods, active bindings or
/// forcing could.
pub unsafe fn quiet_step(machine: &mut Machine<'_>, pc: usize) -> Option<R<()>> {
    let mut state = activation(machine, true);
    // A specialized or fused opcode steps as the instruction it starts from, so the step
    // table is indexed by that base op, never by the specialized handler index.
    let op = op_of(*state.code.add(pc) as u32)?;
    if !runs_no_r_code(&state, op, pc) {
        return None;
    }
    let heap = state.heap;
    let mark = (*heap).handle_mark();
    (*STEP_HANDLERS.as_ptr().add(op as usize))(&mut state, pc);
    (*heap).release_handles(mark);
    Some(state.exit.map(|_| ()))
}

#[inline(always)]
unsafe fn runs_no_r_code(state: &Activation<'_, '_>, op: Op, pc: usize) -> bool {
    if (*state.interp).handlers.is_some() {
        return false;
    }
    let code = state.code;
    let registers = state.registers;
    let operand = |at: usize| *registers.add(*code.add(pc + 1 + at) as usize);
    let plain = |value: Sexp| {
        !matches!(
            value.kind(),
            SexpType::Environment | SexpType::Promise | SexpType::Symbol | SexpType::Language
        ) && !value.is_object()
            && !value.is_altrep()
    };
    if op == Op::CallBuiltin {
        let first = *code.add(pc + 4) as usize;
        let count = *code.add(pc + 5) as usize;
        element_builtin(state, pc).is_some()
            && (first..first + count).all(|register| plain(*registers.add(register)))
    } else if op == Op::Field {
        plain(operand(1))
    } else if op == Op::SubAssign {
        assigned_slot(state, pc).is_some_and(|cell| {
            matches!(
                (*cell).kind(),
                SexpType::Logical
                    | SexpType::Integer
                    | SexpType::Real
                    | SexpType::Complex
                    | SexpType::String
                    | SexpType::List
            ) && plain(*cell)
        }) && plain(operand(2))
            && plain(operand(3))
    } else {
        false
    }
}

/// Where an activation stands after [`step`] ran one of its instructions.
pub enum Stepped {
    /// Interpretation continues at this pc.
    At(usize),
    /// The activation returned this value.
    Returned(Sexp),
}

/// Runs the one instruction at `pc`, of any kind, including jumps, branches and `Return`, and
/// answers where interpretation continues. Stepping a body from pc 0 leaves the registers and
/// loop states exactly as tier 0 has them at every pc it reaches, which lets a test hand the
/// activation back through [`TierExit::Resume`] at each of them.
pub unsafe fn step(machine: &mut Machine<'_>, pc: usize) -> R<Stepped> {
    match interpret::<true>(machine, pc) {
        (Ok(value), STOP) => Ok(Stepped::Returned(value)),
        (Ok(_), next) => Ok(Stepped::At(next)),
        (Err(unwind), _) => continue_after(machine, pc, unwind).map(Stepped::At),
    }
}

/// The frame-position hints of `code`'s lookup slots, one `u32` per slot, which compiled code
/// reads to probe a frame without a search. The vector is never resized.
pub unsafe fn lookup_hints(code: Sexp) -> *mut u32 {
    LookupCache::of(code).hints
}

/// Records `value`'s kind as type feedback for the instruction at `pc`.
pub unsafe fn record_value(code: Sexp, pc: usize, value: Sexp) {
    record_feedback(code.cdr(), pc, value);
}

/// Marks the `GetVar` at `pc` as one that called an active binding.
pub unsafe fn record_active_binding(code: Sexp, pc: usize) {
    if let Some(bits) = values::ints_mut(code.cdr().element(FEEDBACK)).get_mut(pc) {
        *bits |= feedback::ACTIVE_BINDING as i32;
    }
}

/// What `GetVar` at `pc` reads, and whether the binding is in the activation's own frame.
pub unsafe fn load_variable(machine: &Machine<'_>, pc: usize) -> R<(Sexp, bool)> {
    let code = machine.code;
    let words = values::ints(code.car());
    let constants = code.cdr();
    let symbol = constants.element(words[pc + 2] as usize);
    let cache = words[pc + 3] as usize;
    let in_slots = machine.env() == nil();
    let (value, local) = match machine.slot_of(symbol) {
        None if in_slots => {
            let start = machine.function.tag();
            (
                lookup_outside(
                    symbol,
                    start,
                    LexicalFrame::of(machine),
                    LookupCache::of(code),
                    cache,
                    0..0,
                )?,
                false,
            )
        }
        Some(slot) => {
            let value = *slot;
            if value.kind() == SexpType::Promise {
                let forced = if value.car() == unbound() {
                    let forced = eval::force_promise(value)?;
                    crate::heap().root(forced);
                    forced
                } else {
                    value.car()
                };
                (forced, true)
            } else if value == missing_arg() {
                let env = machine.reify(Reify::Site("missing argument lookup"))?;
                (
                    lookup_variable(symbol, env, LookupCache::of(code), cache, 0..0)?,
                    true,
                )
            } else if value != unbound() {
                (value, true)
            } else {
                let start = machine.function.tag();
                (
                    lookup_outside(
                        symbol,
                        start,
                        LexicalFrame::of(machine),
                        LookupCache::of(code),
                        cache,
                        0..0,
                    )?,
                    false,
                )
            }
        }
        None => {
            let env = machine.env();
            let local = crate::heap().frame_cell(env, symbol).is_some();
            (
                lookup_variable(symbol, env, LookupCache::of(code), cache, 0..0)?,
                local,
            )
        }
    };
    record_feedback(constants, pc, value);
    Ok((value, local))
}

/// What every instruction of an activation reads besides its code and registers, loaded once
/// instead of once per instruction. None of these addresses changes while the activation runs:
/// the code object is rooted and never moved (quickening rewrites an op word in place, never the
/// vector), its vectors are never resized, and the register window was reserved on entry in a file that never reallocates. What they point at (registers,
/// cache slots) does change.
struct Activation<'m, 'a> {
    machine: &'m mut Machine<'a>,
    constants: Sexp,
    cache: LookupCache,
    /// The type feedback words, one per code word, or null while nothing will read them: no
    /// optimizing tier is installed, or the code is not yet warm (see [`warm`]).
    feedback: *mut i32,
    /// The header register ([`Machine::header`]), read on every use: a callee may give the
    /// slot frame its environment while the activation runs.
    header: *mut Sexp,
    /// Per lookup slot of the code, the frame slot that holds its symbol or -1; null when the
    /// code has no slot table.
    slot_map: *const i32,
    mark: usize,
    heap: *mut Heap,
    interp: *mut Interp,
    code: *const i32,
    registers: *mut Sexp,
    /// What the activation returns once a handler stops the chain by answering [`STOP`].
    exit: R<Sexp>,
    /// Where a single-stepped instruction continues, or [`STOP`] once the activation finished.
    stepped: usize,
    /// How many [`Resume`] records [`State::resumes`] held when this dispatch loop started:
    /// the records above it are the callers of bodies this loop entered in place.
    /// [`NO_RESUMES`] for an activation that runs single instructions, whose calls nest.
    resume_base: usize,
}

impl Activation<'_, '_> {
    /// The activation's environment, made from its slots first if it has none: what every
    /// instruction that hands the frame to other code reads.
    #[inline(always)]
    unsafe fn env(&self, cause: Reify) -> R<Sexp> {
        let env = *self.header;
        if env != nil() {
            return Ok(env);
        }
        self.machine.reify(cause)
    }

    /// Whether the activation runs in a slot frame that has no environment yet.
    #[inline(always)]
    unsafe fn in_slots(&self) -> bool {
        *self.header == nil()
    }

    /// The frame slot of lookup slot `cache`, if its symbol has one.
    #[inline(always)]
    unsafe fn slot(&self, cache: usize) -> Option<*mut Sexp> {
        let slot = *self.slot_map.add(cache);
        (slot >= 0).then(|| self.header.add(1 + slot as usize))
    }

    #[inline(always)]
    unsafe fn constant(&self, instruction: Instruction, index: usize) -> Sexp {
        self.constants.element(instruction.operand(index))
    }

    /// Records `value`'s kind as feedback for the instruction at `pc`. The kind is computed only
    /// while something records, since it takes several loads and tests.
    #[inline(always)]
    unsafe fn record(&self, pc: usize, value: Sexp) {
        if !self.feedback.is_null() {
            *self.feedback.add(pc) |= value_kind(value) as i32;
        }
    }

    #[inline(always)]
    unsafe fn record_kind(&self, pc: usize, kind: u32) {
        if !self.feedback.is_null() {
            *self.feedback.add(pc) |= kind as i32;
        }
    }
}

/// The instruction an op handler runs: its position in the code and the register window.
#[derive(Clone, Copy)]
struct Instruction {
    code: *const i32,
    at: usize,
    registers: *mut Sexp,
}

impl Instruction {
    #[inline(always)]
    unsafe fn operand(self, index: usize) -> usize {
        *self.code.add(self.at + 1 + index) as usize
    }

    #[inline(always)]
    unsafe fn register(self, index: usize) -> Sexp {
        *self.registers.add(index)
    }

    #[inline(always)]
    unsafe fn set(self, index: usize, value: Sexp) {
        *self.registers.add(index) = value;
    }

    /// The register operand `index` names.
    #[inline(always)]
    unsafe fn input(self, index: usize) -> Sexp {
        self.register(self.operand(index))
    }

    /// Stores into the register operand `index` names.
    #[inline(always)]
    unsafe fn output(self, index: usize, value: Sexp) {
        self.set(self.operand(index), value);
    }
}

/// How interpretation goes on after an instruction.
enum Flow {
    /// With the instruction after it.
    Next,
    /// With the instruction at this position.
    Goto(usize),
    /// Nowhere: the activation returns this value.
    Done(Sexp),
}

/// An op handler. Every instruction ends by tail-calling the handler of the next one, so the
/// interpreter's state stays in argument registers from one instruction to the next instead of
/// living in the stack frame of one function that holds every op, which spilled and reloaded it
/// around each dispatch. A handler that calls out keeps its own frame only for that call.
type Handler = unsafe fn(&mut Activation<'_, '_>, usize);

/// Interprets `machine`'s code from `pc` until it returns, or, when `SINGLE`, runs the one
/// instruction at `pc`.
#[inline(always)]
unsafe fn interpret<const SINGLE: bool>(machine: &mut Machine<'_>, pc: usize) -> (R<Sexp>, usize) {
    let mut state = activation(machine, SINGLE);
    let handlers = if SINGLE {
        &STEP_HANDLERS
    } else {
        &RUN_HANDLERS
    };
    if !SINGLE {
        state.resume_base = (*state.interp).bytecode.resumes.len();
        release(&state);
    }
    (*handlers.as_ptr().add(op_index(state.code, pc)))(&mut state, pc);
    (state.exit, state.stepped)
}

/// Runs the [`Op::Fused`] instruction at `pc` for optimized code, which goes on at the
/// instruction's target when this returns `true` and runs the ordinary code otherwise.
pub unsafe fn fused_step(machine: &mut Machine<'_>, pc: usize) -> R<bool> {
    let mut state = activation(machine, false);
    let instruction = Instruction {
        code: state.code,
        at: pc,
        registers: state.registers,
    };
    Ok(matches!(op_fused(&mut state, instruction)?, Flow::Goto(_)))
}

/// The instruction whose type feedback describes the value of the fused program `program`:
/// its reduction's call, or else its root operator.
pub unsafe fn fused_result_site(program: Sexp) -> Option<usize> {
    if program.kind() != SexpType::Integer {
        return None;
    }
    let words = values::ints(program);
    if words.get(fusion::REDUCTION).copied()? >= 0 {
        return usize::try_from(*words.get(fusion::REDUCTION_SITE)?).ok();
    }
    let root = words.len().checked_sub(fusion::NODE)?;
    usize::try_from(*words.get(root + 4)?).ok()
}

/// The registers the fused program `program` reads: those of the call operands its
/// expression evaluates ahead of the pass.
pub unsafe fn fused_operand_registers(program: Sexp) -> SmallVec<[usize; 4]> {
    if program.kind() != SexpType::Integer {
        return SmallVec::new();
    }
    let words = values::ints(program);
    let count = |index: usize| words.get(index).map_or(0, |&word| word as usize);
    let start = fusion::HEADER + count(fusion::GUARDS) * fusion::GUARD;
    let end = start + count(fusion::LEAVES) * fusion::LEAF;
    words
        .get(start..end)
        .unwrap_or_default()
        .chunks_exact(fusion::LEAF)
        .filter(|leaf| leaf[0] == 2)
        .map(|leaf| leaf[1] as usize)
        .collect()
}

#[inline(always)]
unsafe fn activation<'m, 'a>(machine: &'m mut Machine<'a>, single: bool) -> Activation<'m, 'a> {
    let constants = machine.code.cdr();
    // An instruction compiled code hands back runs here one at a time, and its result is what a
    // failed speculation on it saw. Recording it always lets the recompile see that type, which
    // the deoptimized activation resuming after the instruction never records.
    let feedback = if single || *pool_ints(constants, PROFILE).add(profile::RECORDING) != 0 {
        pool_ints(constants, FEEDBACK)
    } else {
        std::ptr::null_mut()
    };
    let code = machine.code.car().data().cast::<i32>().cast_const();
    let registers = register_address(machine.base);
    let slots = pool_entry(constants, SLOTS);
    let slot_map = if slots == nil() {
        std::ptr::null()
    } else {
        pool_ints(slots, 0).cast_const()
    };
    Activation {
        constants,
        cache: LookupCache::of(machine.code),
        feedback,
        header: register_address(machine.header),
        slot_map,
        mark: machine.mark,
        heap: crate::heap(),
        interp: interp(),
        code,
        registers,
        exit: Ok(nil()),
        stepped: STOP,
        resume_base: NO_RESUMES,
        machine,
    }
}

/// Points `state` at the activation its machine now describes, after the dispatch loop entered
/// a callee in place or returned from one.
#[inline(always)]
unsafe fn reload(state: &mut Activation<'_, '_>) {
    let machine = &*state.machine;
    let constants = machine.code.cdr();
    state.feedback = if *pool_ints(constants, PROFILE).add(profile::RECORDING) != 0 {
        pool_ints(constants, FEEDBACK)
    } else {
        std::ptr::null_mut()
    };
    state.code = machine.code.car().data().cast::<i32>().cast_const();
    state.registers = register_address(machine.base);
    let slots = pool_entry(constants, SLOTS);
    state.slot_map = if slots == nil() {
        std::ptr::null()
    } else {
        pool_ints(slots, 0).cast_const()
    };
    state.constants = constants;
    state.cache = LookupCache::of(machine.code);
    state.header = register_address(machine.header);
    state.mark = machine.mark;
}

/// Forgets the callers of bodies entered in place, for an unwind to top level that skipped
/// their returns.
pub fn forget_resumes(state: &mut State) {
    state.resumes.clear();
    state.loop_states.clear();
}

/// [`Activation::resume_base`] of an activation that enters no callee in place.
const NO_RESUMES: usize = usize::MAX;

/// The caller of a body the dispatch loop entered in place: what [`Machine`] held for it, and
/// what its call instruction still has to do once the callee returns.
struct Resume {
    function: Sexp,
    code: Sexp,
    header: usize,
    base: usize,
    mark: usize,
    loops: *mut [LoopState],
    /// Where the caller's call instruction is in its code.
    call_at: usize,
    /// The callee's frame, which [`eval::leave`] pops.
    depth: u32,
    /// [`Interp::references_used`] before the call collected its arguments.
    references: u32,
    /// How many [`State::loop_states`] were in use before the callee took its own.
    loop_mark: u32,
}

/// Continues the dispatch loop in the body of the call [`eval::enter`] set up as `entered`,
/// for the call instruction at `call_at`, and answers where: the callee's first instruction,
/// or wherever the loop goes on when the optimizing tier ran the body whole.
unsafe fn enter_in_place(
    state: &mut Activation<'_, '_>,
    call_at: usize,
    function: Sexp,
    entered: eval::Entered,
    slots: usize,
    references: u32,
) -> usize {
    switch_to_callee(
        state,
        call_at,
        function,
        entered.header as usize,
        slots,
        entered.depth,
        references,
        loop_count(entered.value),
    );
    let register_count = *pool_ints(state.constants, REGISTERS) as usize;
    if let Err(unwind) = reserve_registers(state.machine.base + register_count) {
        return finished(state, Err(unwind));
    }
    count_entry(state)
}

/// Makes the body of `function` the activation the dispatch loop runs, on the slot frame at
/// `header` whose call frame is at `depth`, and records what the caller's call instruction at
/// `call_at` still has to do once the body returns ([`Resume`]). The caller has checked that
/// the body's `loops` loop states fit ([`in_place`]).
#[allow(clippy::too_many_arguments)]
#[inline(always)]
unsafe fn switch_to_callee(
    state: &mut Activation<'_, '_>,
    call_at: usize,
    function: Sexp,
    header: usize,
    slots: usize,
    depth: u32,
    references: u32,
    loops: usize,
) {
    let code = function.cdr();
    let bytecode = &mut (*state.interp).bytecode;
    let loop_mark = bytecode.loop_states.len();
    if loops != 0 {
        bytecode
            .loop_states
            .extend(std::iter::repeat_n(LoopState::default(), loops));
    }
    let loops: *mut [LoopState] = bytecode.loop_states.get_unchecked_mut(loop_mark..);
    let machine = &mut *state.machine;
    bytecode.resumes.push(Resume {
        function: machine.function,
        code: machine.code,
        header: machine.header,
        base: machine.base,
        mark: machine.mark,
        loops: std::ptr::from_mut(std::mem::replace(&mut machine.loops, &mut *loops)),
        call_at,
        depth,
        references,
        loop_mark: loop_mark as u32,
    });
    machine.function = function;
    machine.code = code;
    machine.header = header;
    machine.base = header + 1 + slots;
    (*state.heap).root(code);
    machine.mark = (*state.heap).handle_mark();
    reload(state);
}

/// Counts an entry of the body the dispatch loop just switched to, and answers where the loop
/// goes on: the body's first instruction, unless the tiers' schedule picked this entry.
#[inline(always)]
unsafe fn count_entry(state: &mut Activation<'_, '_>) -> usize {
    let profile = pool_ints(state.constants, PROFILE);
    let entries = (*profile.add(profile::ENTRIES)).saturating_add(1);
    *profile.add(profile::ENTRIES) = entries;
    if entries < *profile.add(profile::NEXT) {
        return 0;
    }
    scheduled_entry_in_place(state, entries)
}

/// [`run_scheduled_entry`] for a body entered in place.
#[inline(never)]
unsafe fn scheduled_entry_in_place(state: &mut Activation<'_, '_>, entries: i32) -> usize {
    let event = entry_event(state.machine, entries);
    // The tier may have started or stopped the body's recording.
    reload(state);
    match event {
        Some(TierExit::Finished(result)) => finished(state, result),
        Some(TierExit::Resume { pc, pending }) => match pending {
            Some(unwind) => failed::<false>(state, pc, unwind),
            None => pc,
        },
        None => 0,
    }
}

/// Where the dispatch loop goes on once the running activation has ended with `result`: in the
/// caller that entered it in place, after its call instruction or at a loop target the unwind
/// names there, or nowhere ([`STOP`]) when the activation is the one the loop started with or
/// no caller takes the unwind. An in-place callee leaves through here on every way out, as it
/// would leave [`run_at`] and [`eval::leave`] were the call a Rust call.
unsafe fn finished(state: &mut Activation<'_, '_>, result: R<Sexp>) -> usize {
    // Most activations were entered from Rust and have no caller to resume. Testing for that
    // here keeps their exit free of the frame the resuming path needs.
    if (*state.interp).bytecode.resumes.len() <= state.resume_base {
        state.exit = result;
        return STOP;
    }
    resume_caller(state, result)
}

/// [`finished`] for an activation a call instruction entered in place: pops its resume record
/// and goes on in the caller, or in the first caller further out that takes the unwind.
#[inline(never)]
unsafe fn resume_caller(state: &mut Activation<'_, '_>, mut result: R<Sexp>) -> usize {
    loop {
        let bytecode = &mut (*state.interp).bytecode;
        if bytecode.resumes.len() <= state.resume_base {
            state.exit = result;
            return STOP;
        }
        let Some(resume) = bytecode.resumes.pop() else {
            unreachable!("the dispatch loop's resume records are above its base");
        };
        bytecode.registers.truncate(state.machine.base);
        let env = *state.header;
        if env != nil() {
            env.set_gp_bits(gp::COMPILED_FRAME, false);
        }
        if let Err(Unwind::Error(failure)) = result {
            result = Err(conditions::signal_error(failure));
        }
        let returned = eval::leave(resume.depth as usize, result);
        let interp = &mut *state.interp;
        interp.bytecode.loop_states.truncate(resume.loop_mark as usize);
        let machine = &mut *state.machine;
        machine.function = resume.function;
        machine.code = resume.code;
        machine.header = resume.header;
        machine.base = resume.base;
        machine.mark = resume.mark;
        machine.loops = &mut *resume.loops;
        reload(state);
        if interp.references_used != resume.references {
            eval::release_references(resume.references);
        }
        let at = resume.call_at;
        match returned {
            Ok(value) => {
                state.record(at, value);
                *state.registers.add(*state.code.add(at + 1) as usize) = value;
                release(state);
                return after_instruction(state.code, at);
            }
            Err(unwind) => match continue_after(state.machine, at, unwind) {
                Ok(pc) => return pc,
                Err(unwind) => result = Err(unwind),
            },
        }
    }
}

/// Where tier-1 code finds the interpreter state it writes directly.
pub mod interp_layout {
    /// The visibility flag inside `Interp`.
    pub const VISIBLE: usize = std::mem::offset_of!(crate::Interp, visible);
    /// The current srcref inside `Interp`.
    pub const SRCREF: usize = std::mem::offset_of!(crate::Interp, srcref);
}

/// The handler table position of the instruction at `pc`.
#[inline(always)]
unsafe fn op_index(code: *const i32, pc: usize) -> usize {
    let word = *code.add(pc) as u32;
    debug_assert!(op_of(word).is_some());
    (word & OP_MASK) as usize
}

/// Between instructions every live value sits in a register, so what an instruction rooted is
/// released at once. Otherwise an object only a finished call held would stay alive until the
/// body returns, past a later `gc()` and the finalizer GNU R runs there. Only instructions that
/// can root anything check.
#[inline(always)]
unsafe fn release(state: &Activation<'_, '_>) {
    let heap = &mut *state.heap;
    if heap.handle_mark() > state.mark {
        heap.release_handles(state.mark);
    }
}

/// The next pc a step answers when the activation is done; its result is in
/// [`Activation::exit`].
const STOP: usize = usize::MAX;

/// Defines the handler of `$op` around `$body`, which runs the instruction and says how to go
/// on. `$roots` says whether the instruction can root values, which the handler then releases
/// before the next one runs. A handler takes only the activation and the pc, and leaves the
/// activation's result in [`Activation::exit`], so that few values stay live across the calls
/// an instruction makes. A failure goes to [`failed`].
macro_rules! handlers {
    ($($name:ident: $op:ident, $roots:literal, $body:ident;)*) => {
        // Never inlined: a fast handler tail-calls these on its slow path, and inlining one
        // would bring its frame setup back onto the fast path.
        $(
            #[inline(never)]
            unsafe fn $name<const SINGLE: bool>(state: &mut Activation<'_, '_>, at: usize) {
                let instruction = Instruction { code: state.code, at, registers: state.registers };
                let flow = $body(state, instruction);
                let pc = advance::<SINGLE>(state, at, at + 1 + operand_count(Op::$op), flow);
                if SINGLE {
                    state.stepped = pc;
                    return;
                }
                if pc == STOP {
                    return;
                }
                if $roots {
                    release(state);
                }
                become (*RUN_HANDLERS.as_ptr().add(op_index(state.code, pc)))(state, pc)
            }
        )*

        /// The handler of every opcode. A specialized opcode steps as the instruction it
        /// specializes, so single-stepping never depends on what tier 0 specialized.
        const fn handler_table<const SINGLE: bool>() -> [Handler; HANDLER_COUNT] {
            let mut table: [Handler; HANDLER_COUNT] = [jump::<SINGLE>; HANDLER_COUNT];
            $(table[Op::$op as usize] = $name::<SINGLE>;)*
            let mut index = 0;
            while index < QUICK_ARITHMETIC.len() {
                table[OPS.len() + index] = table[Op::Arithmetic as usize];
                index += 1;
            }
            table[POSITIONAL_CALL_BUILTIN] = table[Op::CallBuiltin as usize];
            table
        }
    };
}

#[inline(always)]
unsafe fn advance<const SINGLE: bool>(
    state: &mut Activation<'_, '_>,
    at: usize,
    next: usize,
    flow: R<Flow>,
) -> usize {
    match flow {
        Ok(Flow::Next) => next,
        Ok(Flow::Goto(pc)) => pc,
        Ok(Flow::Done(value)) => finished(state, Ok(value)),
        Err(unwind) => failed::<SINGLE>(state, at, unwind),
    }
}

/// Resumes after a failed instruction at a loop target of a `break` or `next` aimed at this
/// activation (see [`continue_after`]), or stops with the failure.
#[cold]
#[inline(never)]
unsafe fn failed<const SINGLE: bool>(
    state: &mut Activation<'_, '_>,
    at: usize,
    unwind: Unwind,
) -> usize {
    let resumed = if SINGLE {
        Err(unwind)
    } else {
        continue_after(state.machine, at, unwind)
    };
    match resumed {
        Ok(pc) => pc,
        Err(unwind) => finished(state, Err(unwind)),
    }
}

static RUN_HANDLERS: [Handler; HANDLER_COUNT] =
    specialized_table(fast_path_table(handler_table::<false>()));
static STEP_HANDLERS: [Handler; HANDLER_COUNT] = handler_table::<true>();

handlers! {
    run_constant: Constant, false, op_constant;
    run_get_var: GetVar, true, op_get_var;
    run_set_var: SetVar, true, op_set_var;
    run_get_fun: GetFun, true, op_get_fun;
    run_branch_unless_builtin: BranchUnlessBuiltin, false, op_branch_unless_builtin;
    run_call_builtin: CallBuiltin, true, op_call_builtin;
    run_call_builtin_rebinding: CallBuiltinRebinding, true, op_call_builtin_rebinding;
    run_call_generic: CallGeneric, true, op_call_generic;
    run_call_rebinding: CallRebinding, true, op_call_rebinding;
    run_call_tail_update: CallTailUpdate, true, op_call_tail_update;
    run_field: Field, true, op_field;
    run_item: Item, true, op_item;
    run_match_shape: MatchShape, false, op_match_shape;
    run_match_tag: MatchTag, false, op_match_tag;
    run_match_literal: MatchLiteral, true, op_match_literal;
    run_slot: Slot, true, op_slot;
    run_match_fail: MatchFail, true, op_match_fail;
    run_construct: Construct, true, op_construct;
    run_move: Move, false, op_move;
    run_switch_arm: SwitchArm, true, op_switch_arm;
    run_bound_eval: BoundEval, true, op_bound_eval;
    run_match_condition: MatchCondition, true, op_match_condition;
    run_branch_unless_same: BranchUnlessSame, false, op_branch_unless_same;
    run_branch_if_object: BranchIfObject, false, op_branch_if_object;
    run_dispatch_subject: DispatchSubject, true, op_dispatch_subject;
    run_ast: Ast, true, op_ast;
    run_guard: Guard, true, op_guard;
    run_resume: Resume, false, op_resume;
    run_jump_unless: JumpUnless, true, op_jump_unless;
    run_arithmetic: Arithmetic, true, op_arithmetic;
    run_return: Return, false, op_return;
    run_for_init: ForInit, true, op_for_init;
    run_for_step: ForStep, true, op_for_step;
    run_for_end: ForEnd, false, op_for_end;
    run_invisible: Invisible, false, op_invisible;
    run_visible: Visible, false, op_visible;
    run_set_srcref: SetSrcref, true, op_set_srcref;
    run_end_statement: EndStatement, false, op_end_statement;
    run_logic_left: LogicLeft, true, op_logic_left;
    run_logic_right: LogicRight, true, op_logic_right;
    run_closure: Closure, true, op_closure;
    run_prologue: Prologue, true, op_prologue;
    run_sub_assign: SubAssign, true, op_sub_assign;
    run_sub_assign_matrix: SubAssignMatrix, true, op_sub_assign_matrix;
    run_replace: Replace, true, op_replace;
    run_missing: Missing, true, op_missing;
    run_namespace: Namespace, true, op_namespace;
    run_fused: Fused, true, op_fused;
    run_unfused: Unfused, false, op_unfused;
    run_super_assign: SuperAssign, true, op_super_assign;
    run_internal: Internal, true, op_internal;
}

/// `Jump`, the one instruction whose failure resumes somewhere other than after itself: the
/// optimizing tier can take over at a back edge and hand back an unwind from a later position.
/// It also fills the table slots of the ops listed in `handlers!` before they are overwritten.
#[inline(never)]
unsafe fn jump<const SINGLE: bool>(state: &mut Activation<'_, '_>, at: usize) {
    let target = *state.code.add(at + 1) as usize;
    let mut pc = target;
    if target < at {
        if let Err(unwind) = crate::unwind::poll_interrupt() {
            pc = failed::<SINGLE>(state, at, unwind);
        } else if let Some(tier) = &(*state.interp).bytecode.tier
            && back_edge_due(state, tier)
        {
            match back_edge(state, tier, pc) {
                BackEdge::Continue(resume) => pc = resume,
                BackEdge::Finished(result) => pc = finished(state, result),
                BackEdge::Unwound { pc: resume, unwind } => {
                    pc = failed::<SINGLE>(state, resume, unwind);
                }
            }
        }
    }
    if SINGLE {
        state.stepped = pc;
        return;
    }
    if pc == STOP {
        return;
    }
    release(state);
    become (*RUN_HANDLERS.as_ptr().add(op_index(state.code, pc)))(state, pc)
}

// Fast paths and specialized forms.
//
// A handler that runs only straight-line code with no calls compiles without a frame, so its
// `become` is a load and an indirect jump. The generic handlers cannot be frameless, because
// their slow paths call into the evaluator, and LLVM sets up the frame on entry to the whole
// function. So each of the commonest instructions gets a handler that tries a call-free fast
// path and otherwise tail-calls the generic handler, which pays for the frame only on that path.

/// Continues the handler chain at `pc`.
macro_rules! dispatch {
    ($state:expr, $pc:expr) => {
        become (*RUN_HANDLERS.as_ptr().add(op_index($state.code, $pc)))($state, $pc)
    };
}

/// The generic handlers, which the fast paths fall back to.
const GENERIC_HANDLERS: [Handler; HANDLER_COUNT] = handler_table::<false>();

/// Defines a fast handler per op around a fast path, which answers the next pc or `None` to
/// run the generic handler, and the table that puts them in place of the generic ones.
macro_rules! fast_paths {
    ($($name:ident: $op:ident, $fast:ident;)*) => {
        $(
            unsafe fn $name(state: &mut Activation<'_, '_>, at: usize) {
                if let Some(next) = $fast(state, at) {
                    dispatch!(state, next);
                }
                become GENERIC_HANDLERS[Op::$op as usize](state, at)
            }
        )*

        const fn fast_path_table(mut table: [Handler; HANDLER_COUNT]) -> [Handler; HANDLER_COUNT] {
            $(table[Op::$op as usize] = $name;)*
            table[Op::CallGeneric as usize] = fast_call_generic;
            table
        }
    };
}

fast_paths! {
    fast_get_var: GetVar, get_var_fast;
    fast_get_fun: GetFun, get_fun_fast;
    fast_set_var: SetVar, set_var_fast;
    fast_guard: Guard, guard_fast;
    fast_jump_unless: JumpUnless, jump_unless_fast;
    fast_set_srcref: SetSrcref, set_srcref_fast;
    fast_jump: Jump, jump_fast;
    fast_constant: Constant, constant_fast;
    fast_end_statement: EndStatement, end_statement_fast;
    fast_branch_unless_same: BranchUnlessSame, branch_unless_same_fast;
    fast_branch_if_object: BranchIfObject, branch_if_object_fast;
    fast_call_builtin: CallBuiltin, call_builtin_fast;
    fast_sub_assign: SubAssign, sub_assign_fast;
    fast_field: Field, field_fast;
    fast_return: Return, return_fast;
}

/// The fast handler of `CallGeneric`. Unlike the other fast paths, [`call_fast`] can end the
/// activation the loop started with: an error the callee's entry raised, or the optimizing
/// tier running the body whole and finishing it, answers [`STOP`].
unsafe fn fast_call_generic(state: &mut Activation<'_, '_>, at: usize) {
    if let Some(next) = call_fast(state, at) {
        if next == STOP {
            return;
        }
        dispatch!(state, next);
    }
    become GENERIC_HANDLERS[Op::CallGeneric as usize](state, at)
}

/// `CallGeneric` of a closure whose site has a prepared plan ([`prepare_plan`]), from a slot
/// frame, while nothing observes calls: the arguments are written once, onto the frame
/// argument stack, the slots are filled from them by the plan's sources, the call's frame is
/// pushed, and the dispatch loop goes on in the callee's body. Nothing is matched, no Rust
/// frame is entered, and nothing is allocated unless the argument reference pool grows or the
/// callee collects arguments in `...`.
///
/// It does what [`op_call_generic`] does for the same call through [`slot_promises_into`],
/// [`eval::bind_plan`], [`eval::enter`] and [`enter_in_place`], and leaves every call that
/// needs anything else to them: a debugger, a profile, a tracked move or a refused object
/// system ([`activity::hooks`]), a closure flagged for debugging or with a native entry, the
/// namespace loader, `do.call`, a frame stack or register file that is full.
#[inline(always)]
unsafe fn call_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    let interp = &mut *state.interp;
    let heap = state.heap;
    let known = (*heap).known();
    let (nil, unbound, missing_value) = (known.nil, known.unbound, known.missing_arg);
    let code = state.code;
    let feedback = pool_entry(state.constants, FEEDBACK);
    if at + 1 >= feedback.length() as usize
        || state.resume_base == NO_RESUMES
        || *state.header != nil
        || activity::hooks() != 0
    {
        return None;
    }
    let word = *feedback.data().cast::<i32>().add(at + 1);
    let function = *state.registers.add(*code.add(at + 2) as usize);
    let plans = &mut interp.bytecode.call_plans;
    if word <= 0 || word as usize > plans.rows.len() || function.kind() != SexpType::Closure {
        return None;
    }
    debug_assert_eq!(
        crate::observer_bits(),
        0,
        "the observer word is clear while something observes calls"
    );
    let row: *mut PlanRow = plans.rows.as_mut_ptr().add(word as usize - 1);
    let body = function.cdr();
    if (*row).owner != state.constants
        || (*row).site as usize != at
        || (*row).formals != function.car()
        || (*row).body != body
        || (*row).flags & PLAN_FAST == 0
        || function.has_gp(
            gp::CLOSURE_DEBUG | gp::CLOSURE_STEP | gp::CLOSURE_TRACE | gp::NATIVE_ENTRY,
        )
        || function == interp.namespace_loader
        || function == interp.do_call
    {
        return None;
    }
    let shape = (*row).shape;
    let (slots, loops) = (shape.slots as usize, (*row).loops as usize);
    let depth = interp.frames.len();
    let header = interp.bytecode.registers.len();
    let end = header + 1 + slots + shape.registers as usize;
    if depth as u32 >= interp.max_depth
        || end > interp.bytecode.registers.capacity()
        || (loops != 0
            && interp.bytecode.loop_states.len() + loops
                > interp.bytecode.loop_states.capacity())
    {
        return None;
    }
    let call_index = *code.add(at + 3) as usize;
    let references = interp.references_used;
    // The same test as the generic path's, with the callee's answer kept in the plan's row.
    let eager = pool_constant(state, call_index + 2) != nil
        && !conditions::calling_established(&interp.handlers)
        && if (*row).effects_env == function.tag()
            && (*row).effects_epoch == (*heap).binding_epoch()
        {
            (*row).flags & PLAN_EFFECT_FREE != 0
        } else {
            match site_effect_free(row, function) {
                Ok(holds) => holds,
                Err(unwind) => return Some(failed::<false>(state, at, unwind)),
            }
        };
    let count = (*row).arguments as usize;
    let pool = interp.bytecode.call_plans.pool.as_ptr();
    let sources = pool.add((*row).sources_at as usize);
    let sources_len = (*row).sources_len as usize;
    let kinds = sources.add(sources_len);
    let args_start = interp.frame_args.len();
    interp.frame_args.reserve(count);
    // Written past the stack's length, which moves once every argument is there. Until then
    // each value is held by what it was read from: the caller's slots, the code's constants,
    // the reference pool.
    let args = interp.frame_args.as_mut_ptr().add(args_start);
    let owner = interp.slot_owner as usize;
    let compiled = pool_constant(state, call_index + 1);
    let call = pool_constant(state, call_index);
    let mut cell = if compiled == nil { call.cdr() } else { compiled };
    for index in 0..count {
        let expression = cell.car();
        let kind = *kinds.add(index);
        let value = if kind == ARGUMENT_VALUE {
            expression
        } else {
            let eager_value = if !eager || kind == ARGUMENT_CODE {
                None
            } else if kind >= 0 {
                let value = *state.header.add(1 + kind as usize);
                if value == missing_value {
                    None
                } else if value == unbound {
                    effects::eager_argument(expression, state.machine.function.tag())
                } else if value.kind() == SexpType::Promise && value.car() != unbound {
                    Some(value.car())
                } else {
                    Some(value)
                }
            } else {
                effects::eager_argument(expression, state.machine.function.tag())
            };
            match eager_value {
                Some(value) => value,
                None => {
                    let reference = if kind == ARGUMENT_CODE {
                        eval::code_reference(owner, expression)
                    } else {
                        let slot = if kind >= 0 {
                            kind as u32
                        } else {
                            eval::NO_REFERENCE_SLOT
                        };
                        eval::argument_reference(owner, slot, expression)
                    };
                    match reference {
                        Ok(reference) => reference,
                        Err(unwind) => {
                            return Some(call_fast_left(state, at, references, Err(unwind)));
                        }
                    }
                }
            }
        };
        args.add(index).write(Arg {
            tag: cell.tag(),
            value,
        });
        cell = cell.cdr();
    }
    interp.frame_args.set_len(args_start + count);
    let formals = (shape.formals as usize).min(sources_len);
    let mut dots = missing_value;
    for index in (formals..sources_len).rev() {
        let arg = *args.add(*sources.add(index) as usize);
        let rest = if dots == missing_value { nil } else { dots };
        dots = match values::triple(SexpType::Dots, arg.value, rest, arg.tag) {
            Ok(dots) => dots,
            Err(unwind) => {
                interp.frame_args.truncate(args_start);
                return Some(call_fast_left(state, at, references, Err(unwind)));
            }
        };
    }
    // The capacity was checked above, and every register of the window is written here
    // before anything can read it. The body's own registers start as `NULL` so that a
    // collection frees what a returned call left in those words ([`grow_registers`]).
    interp.bytecode.registers.set_len(end);
    let window = interp.bytecode.registers.as_mut_ptr().add(header);
    window.write(nil);
    let (mut missing, mut promised) = (0u64, 0u64);
    let (mut formal, mut formal_index) = (function.car(), 0);
    for index in 0..formals {
        let source = *sources.add(index);
        let value = if source >= 0 {
            (*args.add(source as usize)).value
        } else if source == eval::DOTS_SOURCE {
            dots
        } else if source <= eval::LITERAL_DEFAULT {
            while formal_index < index {
                formal = formal.cdr();
                formal_index += 1;
            }
            missing |= 1 << index;
            if source == eval::PROMISED_DEFAULT {
                promised |= 1 << index;
            }
            formal.car()
        } else {
            missing_value
        };
        value.increment_refcount();
        window.add(1 + index).write(value);
    }
    for index in formals..slots {
        window.add(1 + index).write(unbound);
    }
    for index in 1 + slots..end - header {
        window.add(index).write(nil);
    }
    interp.calls_ever = interp.calls_ever.wrapping_add(1);
    let caller = interp.slot_owner;
    let frame = Frame {
        call,
        function,
        env: nil,
        sys_parent: nil,
        args: args_start as u32,
        on_exit: ThinVec::new(),
        handlers: interp.handlers.clone(),
        restarts: interp.restarts.clone(),
        srcref: interp.srcref,
        method_vars: interp.method_vars.len() as u32,
        closures: interp.frame_closures.len() as u32,
        slots: header as u32,
        missing,
        caller,
        pending: nil,
        unforced: 0,
        inlined: false,
        reified_by: crate::NOT_REIFIED,
        pending_shared: false,
        handle_mark: (*heap).handle_mark() as u32,
        previous_owner: caller,
    };
    interp.frames.push_with(|| frame);
    interp.srcref = if function.attrib() == nil {
        nil
    } else {
        values::get_attr(function, symbols().srcref)
    };
    if promised != 0
        && let Err(unwind) = eval::default_promises(depth, function, header, promised)
    {
        let result = eval::leave(depth + 1, Err(unwind));
        return Some(call_fast_left(state, at, references, result));
    }
    interp.slot_owner = depth as u32;
    switch_to_callee(
        state,
        at,
        function,
        header,
        slots,
        depth as u32 + 1,
        references,
        loops,
    );
    Some(count_entry(state))
}

/// [`effects::effect_free`] for a call of `function` at the site of `row`, which keeps the
/// answer for the next call of the same closure ([`PlanRow::effects_env`]).
#[cold]
#[inline(never)]
unsafe fn site_effect_free(row: *mut PlanRow, function: Sexp) -> R<bool> {
    let (holds, keeps) = effects::effect_free_for_epoch(function)?;
    // Answering can collect, which frees a plan whose keys died, but not this one: its keys
    // are the running code and the closure being called.
    (*row).effects_env = if keeps { function.tag() } else { nil() };
    (*row).effects_epoch = crate::heap().binding_epoch();
    (*row).flags = ((*row).flags & !PLAN_EFFECT_FREE) | if holds { PLAN_EFFECT_FREE } else { 0 };
    Ok(holds)
}

/// Where the dispatch loop goes on when [`call_fast`] could not enter its callee and the call
/// ended with `result` instead: after the call instruction with its value, or wherever the
/// failure is taken.
#[cold]
#[inline(never)]
unsafe fn call_fast_left(
    state: &mut Activation<'_, '_>,
    at: usize,
    references: u32,
    result: R<Sexp>,
) -> usize {
    if (*state.interp).references_used != references {
        eval::release_references(references);
    }
    match result {
        Ok(value) => {
            state.record(at, value);
            *state.registers.add(*state.code.add(at + 1) as usize) = value;
            release(state);
            after_instruction(state.code, at)
        }
        Err(unwind) => failed::<false>(state, at, unwind),
    }
}

/// `Return` from a body the dispatch loop entered in place, when the frame has nothing to run
/// or keep on the way out: no environment, no pending environment, no `on.exit`, and nothing
/// observing calls. It does what [`finished`] does through [`eval::leave`] for such a frame
/// and goes on after the caller's call instruction.
///
/// The value needs no root between the callee's registers and the caller's: giving up the
/// slots' references frees nothing by itself, and nothing here allocates.
#[inline(always)]
unsafe fn return_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    let interp = &mut *state.interp;
    let heap = &mut *state.heap;
    let nil = heap.known().nil;
    let count = interp.bytecode.resumes.len();
    if count <= state.resume_base || *state.header != nil || activity::hooks() != 0 {
        return None;
    }
    let depth = interp.bytecode.resumes.get_unchecked(count - 1).depth as usize;
    if depth != interp.frames.len()
        || !interp.pending_closures.is_empty()
        || crate::annotations::checks_pending()
    {
        return None;
    }
    let frame = &interp.frames[depth - 1];
    if frame.pending != nil || !frame.on_exit.is_empty() {
        return None;
    }
    debug_assert!(frame.env == nil && frame.slots as usize == state.machine.header);
    let value = *state.registers.add(*state.code.add(at + 1) as usize);
    let (args, method_vars, closures) = (frame.args, frame.method_vars, frame.closures);
    heap.release_handles(frame.handle_mark as usize);
    interp.slot_owner = frame.previous_owner;
    interp.srcref = frame.srcref;
    let Some((handlers, restarts)) = interp.frames.pop() else {
        unreachable!("the returning call's frame is on the stack");
    };
    conditions::restore_handlers(handlers);
    interp.restarts = restarts;
    interp.frame_args.truncate(args as usize);
    interp.method_vars.truncate(method_vars as usize);
    interp.frame_closures.truncate(closures as usize);
    let header = state.machine.header;
    let registers = &mut interp.bytecode.registers;
    registers.truncate(state.machine.base);
    heap.release_slots(registers.get_unchecked_mut(header + 1..));
    registers.truncate(header);
    let Some(resume) = interp.bytecode.resumes.pop() else {
        unreachable!("the dispatch loop's resume records are above its base");
    };
    interp.bytecode.loop_states.truncate(resume.loop_mark as usize);
    let machine = &mut *state.machine;
    machine.function = resume.function;
    machine.code = resume.code;
    machine.header = resume.header;
    machine.base = resume.base;
    machine.mark = resume.mark;
    machine.loops = &mut *resume.loops;
    reload(state);
    if interp.references_used != resume.references {
        eval::release_references(resume.references);
    }
    let call_at = resume.call_at;
    state.record(call_at, value);
    *state.registers.add(*state.code.add(call_at + 1) as usize) = value;
    release(state);
    Some(after_instruction(state.code, call_at))
}

/// The constant `index` of the activation's constant pool, read without the ALTREP check of
/// [`Sexp::element`]: the pool is a plain list.
#[inline(always)]
unsafe fn pool_constant(state: &Activation<'_, '_>, index: usize) -> Sexp {
    *state.constants.data().cast::<Sexp>().add(index)
}

/// `GetVar` in a slot frame: a slot's value, or else the cached cell's value of a name the frame
/// does not bind, either a promise's value once forced. What needs the evaluator (an unforced
/// promise, an active binding, a missing argument, a cache miss) runs the generic handler. A
/// cache key is never a function frame, so a hit also means no function frame lies between.
#[inline(always)]
unsafe fn get_var_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    let known = (*state.heap).known();
    if *state.header != known.nil {
        return None;
    }
    let code = state.code;
    let cache_slot = *code.add(at + 3) as usize;
    let slot = *state.slot_map.add(cache_slot);
    let mut value = if slot >= 0 {
        *state.header.add(1 + slot as usize)
    } else {
        let heap = &*state.heap;
        let cache = state.cache;
        let symbol = pool_constant(state, *code.add(at + 2) as usize);
        if *cache.epochs.add(cache_slot) != heap.lookup_stamp(symbol)
            || *cache.slots.add(cache_slot * 2) != state.machine.function.tag()
        {
            return None;
        }
        let cell = *cache.slots.add(cache_slot * 2 + 1);
        if cell.has_gp(gp::ACTIVE_BINDING) {
            return None;
        }
        cell.cdr()
    };
    match value.kind() {
        SexpType::Promise => {
            value = value.car();
            if value == known.unbound {
                return None;
            }
        }
        SexpType::Symbol if value == known.missing_arg || value == known.unbound => return None,
        _ => {}
    }
    state.record(at, value);
    *state.registers.add(*code.add(at + 1) as usize) = value;
    (*state.interp).visible = true;
    Some(at + 1 + operand_count(Op::GetVar))
}

/// `GetFun` in a slot frame: a slot holding a function, or else the cached cell of a name the
/// frame does not bind, while it holds a function. A cache key is never a function frame, so a
/// hit also means no function frame lies between the closure and the cached binding.
#[inline(always)]
unsafe fn get_fun_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    let heap = &*state.heap;
    let known = heap.known();
    if *state.header != known.nil {
        return None;
    }
    let code = state.code;
    let cache_slot = *code.add(at + 3) as usize;
    let slot = *state.slot_map.add(cache_slot);
    let mut value = if slot >= 0 {
        *state.header.add(1 + slot as usize)
    } else {
        let cache = state.cache;
        let symbol = pool_constant(state, *code.add(at + 2) as usize);
        if *cache.epochs.add(cache_slot) != heap.lookup_stamp(symbol)
            || *cache.slots.add(cache_slot * 2) != state.machine.function.tag()
        {
            return None;
        }
        (*cache.slots.add(cache_slot * 2 + 1)).cdr()
    };
    if value.kind() == SexpType::Promise {
        value = value.car();
    }
    if !values::is_function(value) {
        return None;
    }
    *state.registers.add(*code.add(at + 1) as usize) = value;
    Some(at + 1 + operand_count(Op::GetFun))
}

/// `SetVar` of a local in a slot frame, when giving up the old value needs no bookkeeping.
#[inline(always)]
unsafe fn set_var_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    if *state.header != (*state.heap).known().nil {
        return None;
    }
    let code = state.code;
    let slot = *state.slot_map.add(*code.add(at + 3) as usize);
    if slot < 0 {
        return None;
    }
    let slot = state.header.add(1 + slot as usize);
    let value = *state.registers.add(*code.add(at + 2) as usize);
    if !(*state.heap).try_store_slot(&mut *slot, value) {
        return None;
    }
    clear_missing(state, slot);
    (*state.interp).visible = false;
    Some(at + 1 + operand_count(Op::SetVar))
}

/// `Guard` of a name the closure's frame cannot bind. A symbol no attached frame outside the base
/// frames has bound, looked up from an environment whose chain reaches a base frame, resolves to
/// its base binding, so that binding still holding the expected function, directly or as a forced
/// promise, is the whole check, with no stamp or cache to read (see `gp::BOUND_ELSEWHERE` and
/// `gp::DETACHED`). Otherwise the cached cell holding the expected function answers; a cache key
/// is never a function frame, so a hit also means the enclosure is not one.
#[inline(always)]
unsafe fn guard_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    let code = state.code;
    if *code.add(at + 4) != 0 {
        return None;
    }
    let heap = &*state.heap;
    let env = *state.header;
    let enclosure = if env != heap.known().nil {
        env.enclosure()
    } else {
        state.machine.function.tag()
    };
    let symbol = pool_constant(state, *code.add(at + 1) as usize);
    let expected = pool_constant(state, *code.add(at + 2) as usize);
    if !symbol.has_gp(gp::BOUND_ELSEWHERE) && !enclosure.has_gp(gp::DETACHED) {
        // Base is lazy-loaded, so its binding usually holds the forced promise of the function.
        let base = symbol.cdr().cdr();
        if base == expected || (base.kind() == SexpType::Promise && base.car() == expected) {
            return Some(at + 1 + operand_count(Op::Guard));
        }
    }
    let slot = *code.add(at + 3) as usize;
    let cache = state.cache;
    if *cache.epochs.add(slot) != heap.lookup_stamp(symbol)
        || *cache.slots.add(slot * 2) != enclosure
        || (*cache.slots.add(slot * 2 + 1)).cdr() != expected
    {
        return None;
    }
    Some(at + 1 + operand_count(Op::Guard))
}

/// `JumpUnless` on `TRUE` or `FALSE`, the shared logical scalars every comparison answers.
#[inline(always)]
unsafe fn jump_unless_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    let code = state.code;
    let value = *state.registers.add(*code.add(at + 1) as usize);
    let known = (*state.heap).known();
    if value == known.true_value {
        state.record_kind(at, feedback::BRANCH_NOT_TAKEN);
        Some(at + 1 + operand_count(Op::JumpUnless))
    } else if value == known.false_value {
        state.record_kind(at, feedback::BRANCH_TAKEN);
        Some(*code.add(at + 3) as usize)
    } else {
        None
    }
}

/// `SetSrcref` while no debugger asks to stop at the statement.
#[inline(always)]
unsafe fn set_srcref_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    let code = state.code;
    let interp = &mut *state.interp;
    if *code.add(at + 2) == 1 && interp.debugger.is_some() {
        return None;
    }
    interp.srcref = pool_constant(state, *code.add(at + 1) as usize);
    Some(at + 1 + operand_count(Op::SetSrcref))
}

/// `Jump` forward, and backward while no optimizing tier counts back edges and no interrupt is
/// pending.
#[inline(always)]
unsafe fn jump_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    let target = *state.code.add(at + 1) as usize;
    if target < at
        && ((*state.interp).bytecode.tier.is_some()
            || crate::unwind::INTERRUPT_REQUESTED.load(std::sync::atomic::Ordering::Relaxed))
    {
        return None;
    }
    Some(target)
}

#[inline(always)]
unsafe fn constant_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    let code = state.code;
    *state.registers.add(*code.add(at + 1) as usize) =
        pool_constant(state, *code.add(at + 2) as usize);
    (*state.interp).visible = true;
    Some(at + 1 + operand_count(Op::Constant))
}

/// `EndStatement` clearing at most two temporaries besides its target, the usual statement.
#[inline(always)]
unsafe fn end_statement_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    let code = state.code;
    let registers = state.registers;
    let nil = (*state.heap).known().nil;
    let (start, end) = (*code.add(at + 2) as usize, *code.add(at + 3) as usize);
    if end > start + 2 {
        return None;
    }
    *registers.add(*code.add(at + 1) as usize) = nil;
    let mut register = start;
    while register < end {
        *registers.add(register) = nil;
        register += 1;
    }
    Some(at + 1 + operand_count(Op::EndStatement))
}

#[inline(always)]
unsafe fn branch_unless_same_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    let code = state.code;
    if *state.registers.add(*code.add(at + 1) as usize)
        != pool_constant(state, *code.add(at + 2) as usize)
    {
        return Some(*code.add(at + 3) as usize);
    }
    Some(at + 1 + operand_count(Op::BranchUnlessSame))
}

#[inline(always)]
unsafe fn branch_if_object_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    let code = state.code;
    if (*state.registers.add(*code.add(at + 1) as usize)).is_object() {
        return Some(*code.add(at + 2) as usize);
    }
    Some(at + 1 + operand_count(Op::BranchIfObject))
}

/// `CallBuiltin` of the base `[` or `[[` on two positional values, reading the element
/// [`element_access::element_position`] takes without the call. A profile or activity trace
/// being taken sees the full call, as [`eval::call_direct`] arranges for every direct entry.
#[inline(always)]
unsafe fn call_builtin_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    if *state.code.add(at + 5) != 2 {
        return None;
    }
    let double = element_builtin(state, at)?;
    read_element_call::<false>(state, at, double)
}

/// Whether the `CallBuiltin` at `at` calls the base `[[` (`true`) or `[` (`false`).
#[inline(always)]
unsafe fn element_builtin(state: &Activation<'_, '_>, at: usize) -> Option<bool> {
    let function = *state.registers.add(*state.code.add(at + 2) as usize);
    if function.kind() != SexpType::Special {
        return None;
    }
    let symbol = (*state.interp)
        .builtin(function.primitive_index())?
        .symbol?;
    let names = symbols();
    if symbol == names.bracket2 {
        Some(true)
    } else if symbol == names.bracket {
        Some(false)
    } else {
        None
    }
}

/// [`read_element_call`] of a character vector, reached only once the numeric kinds decline.
#[cold]
#[inline(never)]
unsafe fn read_string_element_call(
    state: &mut Activation<'_, '_>,
    at: usize,
    double: bool,
) -> Option<usize> {
    let subject = *state.registers.add(*state.code.add(at + 4) as usize);
    if subject.kind() != SexpType::String {
        return None;
    }
    read_element_call::<true>(state, at, double)
}

/// The rest of [`call_builtin_fast`] once the builtin is `[` (or `[[`, `double`). Out of line
/// because its frame setup would otherwise run for every builtin call the handler declines.
#[inline(never)]
unsafe fn read_element_call<const STRING: bool>(
    state: &mut Activation<'_, '_>,
    at: usize,
    double: bool,
) -> Option<usize> {
    let code = state.code;
    let registers = state.registers;
    let heap = &mut *state.heap;
    let first = *code.add(at + 4) as usize;
    let subject = *registers.add(first);
    let Some(position) = element_access::element_position::<STRING>(
        heap,
        subject,
        *registers.add(first + 1),
        double,
    ) else {
        // A tail call, and the subject's kind is tested only out of line: tested here, it joins
        // the kind dispatch above, which then compiles to a jump table that the numeric read pays
        // for. The ALTREP test is a header bit, not a kind, so it stays out of that dispatch.
        return if subject.is_altrep() {
            read_altrep_element_call(state, at)
        } else if STRING {
            None
        } else {
            read_string_element_call(state, at, double)
        };
    };
    let known = *heap.known();
    let arguments = pool_constant(state, *code.add(at + 3) as usize).cdr();
    let second = arguments.cdr();
    if arguments.tag() != known.nil
        || second.tag() != known.nil
        || arguments.car() == known.missing_arg
        || second.car() == known.missing_arg
        || activity::hooks() != 0
    {
        return None;
    }
    let value = element_access::element_value::<STRING>(heap, subject, position)?;
    state.record(at, value);
    *registers.add(*code.add(at + 1) as usize) = value;
    (*state.interp).visible = true;
    Some(at + 1 + operand_count(Op::CallBuiltin))
}

/// The ALTREP branch of [`read_element_call`]: `x[i]` or `x[[i]]` for a plain numeric scalar
/// within bounds and, without attributes, a compact integer or double sequence, computed from
/// its first element and increment without expanding it, or a deferred string, converting only
/// that element. Any other ALTREP vector declines to the builtin, whose `Elt` read may run a
/// package's R code.
#[cold]
#[inline(never)]
unsafe fn read_altrep_element_call(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    let code = state.code;
    let registers = state.registers;
    let heap = &mut *state.heap;
    let first = *code.add(at + 4) as usize;
    let subject = *registers.add(first);
    let index = *registers.add(first + 1);
    let kind = subject.kind();
    if subject.attrib() != heap.known().nil
        || index.is_altrep()
        || index.attrib() != heap.known().nil
        || index.length() != 1
    {
        return None;
    }
    let sequence = values::compact_sequence(subject);
    let count = match sequence {
        Some((count, _, _)) => count,
        None if kind == SexpType::String
            && subject.tag() == (*state.interp).deferred_string_class =>
        {
            subject.length() as usize
        }
        None => return None,
    };
    let position = match index.kind() {
        SexpType::Integer => f64::from(index.slice::<i32>()[0]),
        SexpType::Real => index.slice::<f64>()[0],
        _ => return None,
    };
    // NA_integer_ and NaN fail the lower bound, and `x[2.7]` truncates to the second element.
    if !(position >= 1.0 && position < count as f64 + 1.0) {
        return None;
    }
    let known = *heap.known();
    let arguments = pool_constant(state, *code.add(at + 3) as usize).cdr();
    let second = arguments.cdr();
    if arguments.tag() != known.nil
        || second.tag() != known.nil
        || arguments.car() == known.missing_arg
        || second.car() == known.missing_arg
        || activity::enabled()
    {
        return None;
    }
    let offset = position as usize - 1;
    let value = match sequence {
        Some((_, start, step)) => {
            let element = start + step * offset as f64;
            let bits = if kind == SexpType::Integer {
                element as i32 as u32 as u64
            } else {
                element.to_bits()
            };
            heap.try_alloc_scalar(kind, [bits, 0])?
        }
        None => {
            // An error converting the element (only running out of memory) declines, and the
            // builtin then converts it again and raises that error.
            let element = values::altrep_string_elt(subject, offset).ok()?;
            let scalar = heap.try_alloc_scalar(kind, [element.as_ptr() as u64, 0])?;
            // The store `Heap::set_element` would count into a fresh vector.
            element.increment_refcount();
            scalar
        }
    };
    state.record(at, value);
    *registers.add(*code.add(at + 1) as usize) = value;
    (*state.interp).visible = true;
    Some(at + 1 + operand_count(Op::CallBuiltin))
}

/// `Field` (`x$name`) on a list that is not an object or ALTREP and has an element named
/// exactly `name`: the first such element, which is what `$` reads before it tries partial
/// matching. Anything else, a record included, runs the generic handler.
#[inline(always)]
unsafe fn field_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    let code = state.code;
    let registers = state.registers;
    let subject = *registers.add(*code.add(at + 2) as usize);
    if subject.kind() != SexpType::List || subject.is_object() || subject.is_altrep() {
        return None;
    }
    let name = pool_constant(state, *code.add(at + 3) as usize).car();
    let position = element_access::named_position(subject, name)?;
    let value = *subject.data().cast::<Sexp>().add(position);
    state.record(at, value);
    *registers.add(*code.add(at + 1) as usize) = value;
    (*state.interp).visible = true;
    Some(at + 1 + operand_count(Op::Field))
}

/// `SubAssign` of `x[i] <- v`, `x[[i]] <- v` or `x$name <- v` on a slot frame's slot that alone
/// holds a double, integer or logical vector or a list that is not an object, an array or
/// ALTREP, at a plain numeric position within bounds or an existing ASCII name. The
/// value is a plain scalar of the vector's own type, anything but `NULL` for a list's `[[` or
/// `$`, and a one-element list or a plain scalar for a list's `[`. The element is written in
/// place, which is what [`assign_slot_element`] does for that case, without its call. Owning
/// is [`slot_owns`]'s rule for the innermost frame's body, and no move may be in progress, since
/// [`moves::modifying`] would end it.
#[inline(always)]
unsafe fn sub_assign_fast(state: &mut Activation<'_, '_>, at: usize) -> Option<usize> {
    let code = state.code;
    let registers = state.registers;
    let element_kind = *code.add(at + 6);
    if element_kind > 2 {
        return None;
    }
    let target = *assigned_slot(state, at)?;
    let heap = &mut *state.heap;
    let known = *heap.known();
    let value = *registers.add(*code.add(at + 4) as usize);
    let kind = target.kind();
    if !matches!(
        kind,
        SexpType::Real | SexpType::Integer | SexpType::Logical | SexpType::List
    ) {
        return None;
    }
    // The subscript is checked first: one past the end, which appends, is the common decline.
    let index = *registers.add(*code.add(at + 3) as usize);
    let position = if index.kind() == SexpType::String {
        element_access::name_position(heap, target, index)?
    } else {
        element_access::scalar_position(index, target.length() as usize)?
    };
    let plain_scalar =
        |value: Sexp| value.length() == 1 && value.attrib() == known.nil && !value.is_altrep();
    let element = match (kind, element_kind) {
        (SexpType::Real | SexpType::Integer | SexpType::Logical, 0 | 1) => {
            (value.kind() == kind && plain_scalar(value)).then_some(value)?
        }
        // `x[[i]] <- NULL` and `x$name <- NULL` delete the element.
        (SexpType::List, 1 | 2) => (value != known.nil).then_some(value)?,
        // `x[i] <- v` stores a one-element list's element, or a plain scalar itself.
        (SexpType::List, _) => match value.kind() {
            SexpType::List if plain_scalar(value) => value.element(0),
            SexpType::Real | SexpType::Integer | SexpType::Logical | SexpType::String
                if plain_scalar(value) =>
            {
                value
            }
            _ => return None,
        },
        _ => return None,
    };
    if value == target
        || element == target
        || target.refcount() != 1
        || target.is_object()
        || target.is_array()
        || target.is_altrep()
    {
        return None;
    }
    let interp = &*state.interp;
    let header = state.header.offset_from(interp.bytecode.registers.as_ptr()) as usize;
    if state.machine.function == known.nil
        || moves::tracking()
        || !interp.frames.last().is_some_and(|frame| {
            frame.slots as usize == header && frame.function == state.machine.function
        })
    {
        return None;
    }
    let data = target.data();
    match kind {
        SexpType::Real => *data.cast::<f64>().add(position) = *element.data().cast::<f64>(),
        SexpType::List => heap.set_element(target, position, element),
        _ => *data.cast::<i32>().add(position) = *element.data().cast::<i32>(),
    }
    *registers.add(*code.add(at + 1) as usize) = value;
    (*state.interp).visible = false;
    Some(at + 1 + operand_count(Op::SubAssign))
}

/// The cell of the slot frame that holds the variable `SubAssign` at `at` assigns, when the
/// activation runs on a slot frame and the variable is one of its slots.
#[inline(always)]
unsafe fn assigned_slot(state: &Activation<'_, '_>, at: usize) -> Option<*mut Sexp> {
    let known = *(*state.heap).known();
    if *state.header != known.nil {
        return None;
    }
    let by_constant = pool_constant(state, SLOTS).element(3);
    if by_constant == known.nil {
        return None;
    }
    let slot = *values::ints(by_constant).get(*state.code.add(at + 2) as usize)?;
    if slot < 0 {
        return None;
    }
    Some(state.header.add(1 + slot as usize))
}

/// The handler of each specialized arithmetic form, in place of the generic `Arithmetic`'s.
const fn specialized_table(mut table: [Handler; HANDLER_COUNT]) -> [Handler; HANDLER_COUNT] {
    macro_rules! forms {
        ($($index:literal),*) => {
            $(table[OPS.len() + $index] = quick_arithmetic::<$index>;)*
        };
    }
    forms!(
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18
    );
    table[POSITIONAL_CALL_BUILTIN] = positional_call_builtin;
    table
}

/// The handler of [`POSITIONAL_CALL_BUILTIN`]: the element read of `[` and `[[` first, as for the
/// generic instruction, then [`op_call_builtin_positional`].
unsafe fn positional_call_builtin(state: &mut Activation<'_, '_>, at: usize) {
    if let Some(next) = call_builtin_fast(state, at) {
        dispatch!(state, next);
    }
    let instruction = Instruction {
        code: state.code,
        at,
        registers: state.registers,
    };
    let flow = op_call_builtin_positional(state, instruction);
    let pc = advance::<false>(state, at, at + 1 + operand_count(Op::CallBuiltin), flow);
    if pc == STOP {
        return;
    }
    release(state);
    dispatch!(state, pc)
}

/// Whether `count` untagged, non-empty cells make up `call`'s arguments, which lets its
/// `CallBuiltin` take [`POSITIONAL_CALL_BUILTIN_WORD`].
unsafe fn positional_call(call: Sexp, count: usize) -> bool {
    let mut cell = call.cdr();
    let mut cells = 0;
    while cell != nil() && cell.tag() == nil() && cell.car() != missing_arg() {
        cell = cell.cdr();
        cells += 1;
    }
    cell == nil() && cells == count
}

/// The opcode of the specialized form of `operator` on two integers or on doubles, if there is
/// one.
fn quick_form(operator: Arith, real: bool) -> Option<u32> {
    QUICK_ARITHMETIC
        .iter()
        .position(|&form| form == (operator, real))
        .map(|index| (OPS.len() + index) as u32)
}

/// Whether `left` and `right` are plain scalars of the kinds a form takes: two integers, or
/// for a double form, doubles or integers with at least one double.
#[inline(always)]
unsafe fn form_fits(left: Sexp, right: Sexp, real: bool, nil: Sexp) -> bool {
    if !real {
        return left.is_plain_scalar(SexpType::Integer, nil)
            && right.is_plain_scalar(SexpType::Integer, nil);
    }
    let left_real = left.is_plain_scalar(SexpType::Real, nil);
    let right_real = right.is_plain_scalar(SexpType::Real, nil);
    (left_real || left.is_plain_scalar(SexpType::Integer, nil))
        && (right_real || right.is_plain_scalar(SexpType::Integer, nil))
        && (left_real || right_real)
}

/// The specialized form `FORM` of `Arithmetic` (see [`QUICK_ARITHMETIC`]). It computes exactly
/// what [`fast_arithmetic`] does on the same operands, and leaves to the generic handler what
/// that leaves to the builtin: an integer result that is NA or overflows, and an allocation the
/// young generation has no room for.
unsafe fn quick_arithmetic<const FORM: usize>(state: &mut Activation<'_, '_>, at: usize) {
    let (operator, real) = QUICK_ARITHMETIC[FORM];
    let code = state.code;
    let registers = state.registers;
    let left = *registers.add(*code.add(at + 3) as usize);
    let right = *registers.add(*code.add(at + 4) as usize);
    let heap = &mut *state.heap;
    let known = *heap.known();
    if !form_fits(left, right, real, known.nil) {
        become arithmetic_miss(state, at);
    }
    let logical = |result: Option<bool>| match result {
        Some(true) => Some(known.true_value),
        Some(false) => Some(known.false_value),
        None => Some(known.logical_na_value),
    };
    let (value, kind) = if real {
        let as_real = |value: Sexp| {
            if value.kind() == SexpType::Integer {
                let integer = *value.data().cast::<i32>();
                if integer == NA_INTEGER {
                    NA_REAL
                } else {
                    f64::from(integer)
                }
            } else {
                *value.data().cast::<f64>()
            }
        };
        let (a, b) = (as_real(left), as_real(right));
        let nan = a.is_nan() || b.is_nan();
        let mut number = |value: f64| heap.try_alloc_scalar(SexpType::Real, [value.to_bits(), 0]);
        match operator {
            Arith::Add => (number(crate::arith::add(a, b)), feedback::DOUBLE_SCALAR),
            Arith::Subtract => (number(a - b), feedback::DOUBLE_SCALAR),
            Arith::Multiply => (
                number(crate::arith::multiply(a, b)),
                feedback::DOUBLE_SCALAR,
            ),
            Arith::Divide => (number(a / b), feedback::DOUBLE_SCALAR),
            Arith::Equal => (logical((!nan).then_some(a == b)), feedback::LOGICAL_SCALAR),
            Arith::NotEqual => (logical((!nan).then_some(a != b)), feedback::LOGICAL_SCALAR),
            Arith::Less => (logical((!nan).then_some(a < b)), feedback::LOGICAL_SCALAR),
            Arith::Greater => (logical((!nan).then_some(a > b)), feedback::LOGICAL_SCALAR),
            Arith::LessEqual => (logical((!nan).then_some(a <= b)), feedback::LOGICAL_SCALAR),
            _ => (logical((!nan).then_some(a >= b)), feedback::LOGICAL_SCALAR),
        }
    } else {
        let a = *left.data().cast::<i32>();
        let b = *right.data().cast::<i32>();
        let na = a == NA_INTEGER || b == NA_INTEGER;
        let mut integer = |result: Option<i32>| match result {
            Some(value) if !na && value != NA_INTEGER => {
                heap.try_alloc_scalar(SexpType::Integer, [value as u32 as u64, 0])
            }
            _ => None,
        };
        match operator {
            Arith::Add => (integer(a.checked_add(b)), feedback::INTEGER_SCALAR),
            Arith::Subtract => (integer(a.checked_sub(b)), feedback::INTEGER_SCALAR),
            Arith::Multiply => (integer(a.checked_mul(b)), feedback::INTEGER_SCALAR),
            Arith::Equal => (logical((!na).then_some(a == b)), feedback::LOGICAL_SCALAR),
            Arith::NotEqual => (logical((!na).then_some(a != b)), feedback::LOGICAL_SCALAR),
            Arith::Less => (logical((!na).then_some(a < b)), feedback::LOGICAL_SCALAR),
            Arith::Greater => (logical((!na).then_some(a > b)), feedback::LOGICAL_SCALAR),
            Arith::LessEqual => (logical((!na).then_some(a <= b)), feedback::LOGICAL_SCALAR),
            _ => (logical((!na).then_some(a >= b)), feedback::LOGICAL_SCALAR),
        }
    };
    let Some(value) = value else {
        become run_arithmetic::<false>(state, at);
    };
    state.record_kind(at, kind);
    let destination = *code.add(at + 2);
    *registers.add(destination as usize) = value;
    (*state.interp).visible = true;
    let next = at + 1 + operand_count(Op::Arithmetic);
    // A comparison that the next instruction branches on, as every loop test and most `if`
    // tests are, takes the branch itself and saves that instruction's dispatch. An NA leaves
    // the error to the branch.
    if kind == feedback::LOGICAL_SCALAR
        && *code.add(next) as u32 == op_word(Op::JumpUnless)
        && *code.add(next + 1) == destination
    {
        if value == known.true_value {
            state.record_kind(next, feedback::BRANCH_NOT_TAKEN);
            dispatch!(state, next + 1 + operand_count(Op::JumpUnless));
        }
        if value == known.false_value {
            state.record_kind(next, feedback::BRANCH_TAKEN);
            dispatch!(state, *code.add(next + 3) as usize);
        }
    }
    dispatch!(state, next);
}

/// A specialized form whose operands are no longer of its kinds: the site goes back to the
/// generic instruction, counting the undo, and runs it.
#[cold]
#[inline(never)]
unsafe fn arithmetic_miss(state: &mut Activation<'_, '_>, at: usize) {
    let word = state.code.add(at).cast_mut();
    let undone = ((*word as u32) >> RESPECIALIZE_SHIFT) + 1;
    *word = (op_word(Op::Arithmetic) | undone << RESPECIALIZE_SHIFT) as i32;
    become run_arithmetic::<false>(state, at)
}

/// Writes the specialized form for `left` and `right` over the generic `Arithmetic` at `at`,
/// unless the site has undone its specializations too often.
#[inline(always)]
unsafe fn quicken_arithmetic(
    code: *const i32,
    at: usize,
    operator: Arith,
    left: Sexp,
    right: Sexp,
) {
    let word = code.add(at).cast_mut();
    let current = *word as u32;
    if current & OP_MASK != Op::Arithmetic as u32
        || current >> RESPECIALIZE_SHIFT >= RESPECIALIZE_LIMIT
    {
        return;
    }
    let real = left.kind() == SexpType::Real || right.kind() == SexpType::Real;
    if let Some(form) = quick_form(operator, real)
        && form_fits(left, right, real, nil())
    {
        *word = ((current & !OP_MASK) | form) as i32;
    }
}

#[inline(always)]
unsafe fn op_constant(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    instruction.output(0, state.constant(instruction, 1));
    (*state.interp).visible = true;
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_get_var(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    // Visible before the lookup, so that forcing a promise to `invisible()` leaves the value
    // invisible, as `withCallingHandlers(expr)` relies on.
    (*state.interp).visible = true;
    let symbol = state.constant(instruction, 1);
    let base = state.machine.base;
    let held = base..base + instruction.operand(0);
    let value = if state.in_slots() {
        slot_variable(state, symbol, instruction.operand(2), held)?
    } else {
        lookup_variable(
            symbol,
            *state.header,
            state.cache,
            instruction.operand(2),
            held,
        )?
    };
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_set_var(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let symbol = state.constant(instruction, 0);
    let value = instruction.input(1);
    if state.in_slots()
        && let Some(slot) = state.slot(instruction.operand(2))
    {
        (*state.heap).store_slot(&mut *slot, value);
        clear_missing(state, slot);
        (*state.interp).visible = false;
        return Ok(Flow::Next);
    }
    let env = state.env(Reify::Site("assignment"))?;
    let hint = &mut *state.cache.hints.add(instruction.operand(2));
    let cell = (*state.heap).frame_cell_hinted(env, symbol, hint);
    eval::assign_local(symbol, value, env, cell)?;
    (*state.interp).visible = false;
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_get_fun(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let symbol = state.constant(instruction, 1);
    let call = state.constant(instruction, 3);
    let cache = instruction.operand(2);
    let function = if state.in_slots() {
        slot_function(state, symbol, cache, call)?
    } else {
        lookup_function(symbol, *state.header, state.cache, cache, call)?
    };
    instruction.output(0, function);
    Ok(Flow::Next)
}

/// [`lookup_variable`] in a slot frame: the slot's value when the symbol has a bound slot, and
/// otherwise the lookup from the enclosure, which is where the heap frame's lookup would go on
/// to. A slot that holds the empty argument makes the frame's environment first, so the error
/// is the heap frame's.
#[inline(always)]
unsafe fn slot_variable(
    state: &Activation<'_, '_>,
    symbol: Sexp,
    cache: usize,
    held: Range<usize>,
) -> R<Sexp> {
    if let Some(slot) = state.slot(cache) {
        let value = *slot;
        if value.kind() == SexpType::Promise {
            if value.car() != unbound() {
                return Ok(value.car());
            }
            let forced = holding(held, || eval::force_promise(value))?;
            (*state.heap).root(forced);
            return Ok(forced);
        }
        if value == missing_arg() {
            let env = state.env(Reify::Site("variable lookup"))?;
            return lookup_variable(symbol, env, state.cache, cache, held);
        }
        if value != unbound() {
            return Ok(value);
        }
    }
    lookup_outside(
        symbol,
        state.machine.function.tag(),
        LexicalFrame::of(state.machine),
        state.cache,
        cache,
        held,
    )
}

/// [`lookup_function`] in a slot frame: the slot's value when it is a function, and otherwise
/// the lookup from the enclosure, since GNU R skips a binding to anything else.
#[inline(always)]
unsafe fn slot_function(
    state: &Activation<'_, '_>,
    symbol: Sexp,
    cache: usize,
    call: Sexp,
) -> R<Sexp> {
    if let Some(slot) = state.slot(cache) {
        let mut value = *slot;
        if value == missing_arg() {
            let env = state.env(Reify::Site("function lookup"))?;
            return lookup_function(symbol, env, state.cache, cache, call);
        }
        if value.kind() == SexpType::Promise {
            value = if value.car() != unbound() {
                value.car()
            } else {
                let forced = eval::force_promise(value)?;
                (*state.heap).root(forced);
                forced
            };
        }
        if values::is_function(value) {
            return Ok(value);
        }
    }
    let enclosure = state.machine.function.tag();
    let hint = &mut *state.cache.hints.add(cache);
    let Ok(outside) = outside_function_frames(symbol, enclosure, hint) else {
        return eval::find_called_fun(symbol, enclosure, call);
    };
    lookup_function_outside(symbol, enclosure, outside, state.cache, cache, call)
}

/// Clears the missing flag of the formal in `slot`, as an assignment to a formal's binding
/// cell does.
#[inline(always)]
unsafe fn clear_missing(state: &Activation<'_, '_>, slot: *mut Sexp) {
    let formal = slot.offset_from(state.header) as usize - 1;
    if formal < 64 {
        let frames = &mut (*state.interp).frames;
        if let Some(frame) = frames.last_mut()
            && frame.missing != 0
        {
            frame.missing &= !(1 << formal);
        }
    }
}

#[inline(always)]
unsafe fn op_branch_unless_builtin(
    _state: &mut Activation<'_, '_>,
    instruction: Instruction,
) -> R<Flow> {
    if instruction.input(0).kind() != SexpType::Builtin {
        return Ok(Flow::Goto(instruction.operand(1)));
    }
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_call_builtin(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let function = instruction.input(1);
    let call = state.constant(instruction, 2);
    let first = instruction.operand(3);
    let count = instruction.operand(4);
    // The step table, which the optimizing tier runs single instructions through, sends a
    // positional site here too.
    if *instruction.code.add(instruction.at) as u32 == POSITIONAL_CALL_BUILTIN_WORD {
        return op_call_builtin_positional(state, instruction);
    }
    let definition = eval::builtin_def(function)?;
    if count == 1
        && let Some(unary) = definition.unary
        && call.cdr().tag() == nil()
        && call.cdr().car() != missing_arg()
        && let Some(value) = unary(instruction.register(first))?
    {
        (*state.interp).visible = true;
        state.record(instruction.at, value);
        instruction.output(0, value);
        return Ok(Flow::Next);
    }
    if definition.direct.is_some()
        && !definition.checks.replacement
        && positional_arguments(call.cdr())
    {
        let values = std::slice::from_raw_parts(instruction.registers.add(first), count);
        if let Some(value) = eval::call_direct(definition, call, values)? {
            state.record(instruction.at, value);
            instruction.output(0, value);
            return Ok(Flow::Next);
        }
    }
    let mut args = eval::Args::new();
    let mut cell = call.cdr();
    for index in 0..count {
        let value = instruction.register(first + index);
        // Only an empty argument is an error: a promise may well evaluate to the empty symbol,
        // as `X[[i]]` does for a formal without a default.
        if cell.car() == missing_arg() && !definition.keep_missing {
            return Err(error_in(call, format!("argument {} is empty", index + 1)));
        }
        args.push(Arg {
            tag: cell.tag(),
            value,
        });
        cell = cell.cdr();
    }
    let value = call_builtin(definition, call, function, &args, *state.header)?;
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

/// [`op_call_builtin`] at a [`POSITIONAL_CALL_BUILTIN`] site, whose arguments need neither their
/// tags read nor an empty one rejected.
#[inline(always)]
unsafe fn op_call_builtin_positional(
    state: &mut Activation<'_, '_>,
    instruction: Instruction,
) -> R<Flow> {
    let function = instruction.input(1);
    let call = state.constant(instruction, 2);
    let values = std::slice::from_raw_parts(
        instruction.registers.add(instruction.operand(3)),
        instruction.operand(4),
    );
    let definition = eval::builtin_def(function)?;
    if let [value] = values
        && let Some(unary) = definition.unary
        && let Some(value) = unary(*value)?
    {
        (*state.interp).visible = true;
        state.record(instruction.at, value);
        instruction.output(0, value);
        return Ok(Flow::Next);
    }
    if definition.direct.is_some()
        && !definition.checks.replacement
        && let Some(value) = eval::call_direct(definition, call, values)?
    {
        state.record(instruction.at, value);
        instruction.output(0, value);
        return Ok(Flow::Next);
    }
    // The arguments copy the registers `first..first + count`, which keep the same values,
    // rooted, until this instruction writes its result: a nested call's registers start above
    // this frame's, and growing the register file carries them over. So they need no root of
    // their own.
    let env = *state.header;
    // Every dispatch needs an object (or an S4 value, for `c`) among the arguments, except the
    // matrix products' argument count error.
    let dispatches = definition.generic != Generic::None
        && (definition.generic == Generic::MatrixOps
            || values
                .iter()
                .any(|value| value.is_object() || value.has_gp(gp::S4_OBJECT)));
    let value = eval::with_positional_args(
        values.len(),
        |index| {
            values
                .get(index)
                .copied()
                .ok_or_else(|| error("a register is out of range"))
        },
        |args| {
            if dispatches {
                call_builtin(definition, call, function, args, env)
            } else {
                eval::call_builtin_positional(definition, call, function, args, env)
            }
        },
    )?;
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

/// Whether every argument cell of a call is untagged and not empty, as a
/// [`BuiltinDef::direct`] entry requires.
#[inline]
unsafe fn positional_arguments(mut cell: Sexp) -> bool {
    while cell != nil() {
        if cell.tag() != nil() || cell.car() == missing_arg() {
            return false;
        }
        cell = cell.cdr();
    }
    true
}

#[inline(always)]
unsafe fn op_prologue(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let function = state.constant(instruction, 1);
    let call = state.constant(instruction, 2);
    let env = *state.header;
    let frame = crate::base_fast_paths::ClosureFrame::running(env, interp().slot_owner);
    // The prologue may run R code that runs other prologues, each of which puts back the frame
    // it found.
    let outer = interp().prologue.replace(frame);
    let result = call_builtin(eval::builtin_def(function)?, call, function, &[], env);
    let finished = interp().prologue.is_none();
    interp().prologue = outer;
    let value = result?;
    instruction.output(0, value);
    Ok(if finished {
        Flow::Done(value)
    } else {
        Flow::Next
    })
}

#[inline(always)]
unsafe fn op_call_generic(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let function = instruction.input(1);
    let call = state.constant(instruction, 2);
    let value = if function.kind() == SexpType::Closure {
        let arguments = closure_arguments(state, instruction, call)?;
        if function.has_gp(gp::NATIVE_ENTRY)
            && let Some(value) = if state.in_slots() {
                crate::base_fast_paths::native_entry_by_value(
                    function,
                    arguments,
                    true,
                    |symbol| slot_eager_argument(state, symbol),
                )?
            } else {
                let env = state.env(Reify::Site("generic call: native entry"))?;
                crate::base_fast_paths::native_entry_by_value(
                    function,
                    arguments,
                    true,
                    |symbol| effects::eager_argument(symbol, env),
                )?
            }
        {
            state.record(instruction.at, value);
            instruction.output(0, value);
            return Ok(Flow::Next);
        }
        let mut args = eval::Args::new();
        // Binding a variable's value instead of a promise for it is invisible when the callee
        // cannot assign anything before forcing it, nor look at the promise (`substitute`,
        // `missing`), and no handler can run R code in between.
        let eager = state.constants.element(instruction.operand(2) + 2) != nil()
            && !effects::calling_handler_established()
            && effects::effect_free(function)?;
        let plan = direct_plan(state, instruction, function, call)?;
        let plans = &(*state.interp).bytecode.call_plans;
        let generation = plans.generation;
        let per_call = plan.is_some_and(|id| plans.sources(id) == [eval::MATCH_PER_CALL]);
        if let Some(id) = plan
            && !per_call
            && (function == (*state.interp).do_call || plans.pins[id as usize] != nil())
        {
            // Copied out of the table, which the call may grow.
            let sources: SmallVec<[i32; 8]> = SmallVec::from_slice(plans.sources(id));
            if let Some(value) = match call_do_call(state, function, call, &sources)? {
                Some(value) => Some(value),
                None => call_wrapper(state, function, call, id, &sources)?,
            } {
                state.record(instruction.at, value);
                instruction.output(0, value);
                return Ok(Flow::Next);
            }
        }
        if let Some(id) = plan
            && !per_call
            && state.in_slots()
            && function != (*state.interp).do_call
        {
            let plans = &(*state.interp).bytecode.call_plans;
            // The site's compiled arguments are settled once it has run on a slot frame.
            if plans.rows[id as usize].flags & PLAN_PREPARED == 0
                && plans.pins[id as usize] == nil()
                && state.constants.element(instruction.operand(2) + 1) != unbound()
            {
                prepare_plan(state, id, arguments);
            }
        }
        if syntax::testing::invariants_enabled() {
            let observers = activity::DEBUGGER | activity::MOVES | activity::NO_OBJECT_SYSTEMS;
            assert_eq!(
                activity::hooks() & observers,
                crate::observer_bits(),
                "the observer word that a call's fast path tests is out of step with the \
                 interpreter's state"
            );
        }
        let references = (*state.interp).references_used;
        // From a slot frame, a call whose arguments need no promise over the frame leaves it
        // without an environment, and the callee finds its caller as the frame below its own.
        let sys_parent = if state.in_slots()
            && slot_promises_into(state, arguments, eager, plan.is_some(), &mut args)?
        {
            nil()
        } else {
            args.clear();
            let env = state.env(Reify::Site("generic call: arguments"))?;
            if eager {
                eval::eager_args_into(arguments, env, &mut args)?;
            } else if plan.is_some() {
                eval::referenced_args_into(arguments, env, &mut args)?;
            } else {
                eval::compiled_args_into(arguments, env, &mut args)?;
            }
            env
        };
        let result = match plan {
            Some(_) if per_call => eval::call_matching(call, function, &args, sys_parent),
            // The sources are read from the table only once the arguments are collected, and
            // binding them grows no table.
            Some(id) => match (*state.interp).bytecode.call_plans.current(
                id,
                generation,
                state.constants,
                instruction.at,
                function,
            ) {
                Some((shape, sources)) => {
                    if in_place(state, function) {
                        return Ok(Flow::Goto(
                            match eval::bind_plan(call, function, shape, sources, &args, sys_parent)
                            {
                                Ok(entered) if entered.header == crate::NO_SLOTS => {
                                    after_call(state, instruction, references, entered.value)
                                }
                                Ok(entered) => enter_in_place(
                                    state,
                                    instruction.at,
                                    function,
                                    entered,
                                    shape.slots as usize,
                                    references,
                                ),
                                Err(unwind) => {
                                    if (*state.interp).references_used != references {
                                        eval::release_references(references);
                                    }
                                    return Err(unwind);
                                }
                            },
                        ));
                    }
                    eval::call_with_plan(call, function, shape, sources, &args, sys_parent)
                }
                None => match eval::direct_sources(function, call)? {
                    Some((shape, sources)) if sources[..] != [eval::MATCH_PER_CALL] => {
                        eval::call_with_plan(call, function, shape, &sources, &args, sys_parent)
                    }
                    _ => eval::call_matching(call, function, &args, sys_parent),
                },
            },
            None if function.cdr().kind() == SexpType::Bytecode && in_place(state, function) => {
                return Ok(Flow::Goto(
                    match eval::apply_closure_entering(call, function, &args, sys_parent, None) {
                        Ok(entered) if entered.header == crate::NO_SLOTS => {
                            after_call(state, instruction, references, entered.value)
                        }
                        Ok(entered) => enter_in_place(
                            state,
                            instruction.at,
                            function,
                            entered,
                            slot_count(function.cdr()),
                            references,
                        ),
                        Err(unwind) => {
                            if (*state.interp).references_used != references {
                                eval::release_references(references);
                            }
                            return Err(unwind);
                        }
                    },
                ));
            }
            None => eval::apply_closure_owning(call, function, &args, sys_parent, None),
        };
        if (*state.interp).references_used != references {
            eval::release_references(references);
        }
        result?
    } else if state.in_slots()
        && let Some(value) = slot_substitute(state, function, call)?
    {
        value
    } else if state.in_slots()
        && function.kind() == SexpType::Special
        && let Some(value) = slot_special_call(state, instruction, function, call)?
    {
        value
    } else if state.in_slots()
        && function.kind() == SexpType::Builtin
        && let Some(value) = slot_builtin_call(state, call, function, None)?
    {
        value
    } else {
        // A special reads its arguments as expressions, so they stay uncompiled. `pkg::name`
        // reads only the two names, never the frame, so a slot frame needs no environment
        // for it.
        let head = call.car();
        let env = if state.in_slots()
            && function.kind() == SexpType::Special
            && (head == symbols().double_colon || head == symbols().triple_colon)
        {
            crate::heap().known().base_env
        } else {
            state.env(Reify::Site("generic call: special"))?
        };
        eval::apply_call(call, function, call.cdr(), env)?
    };
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

/// Whether the dispatch loop may run the body of `function`, which a call instruction is about
/// to call through its plan, in place of calling it: the loop runs whole activations, and the
/// body's loop states fit [`State::loop_states`].
#[inline(always)]
unsafe fn in_place(state: &Activation<'_, '_>, function: Sexp) -> bool {
    let loops = &(*state.interp).bytecode.loop_states;
    state.resume_base != NO_RESUMES
        && loops.len() + loop_count(function.cdr()) <= loops.capacity()
}

/// What a `CallGeneric` instruction does with the value of a call that ran whole: the pc it
/// goes on at.
#[inline(always)]
unsafe fn after_call(
    state: &mut Activation<'_, '_>,
    instruction: Instruction,
    references: u32,
    value: Sexp,
) -> usize {
    if (*state.interp).references_used != references {
        eval::release_references(references);
    }
    state.record(instruction.at, value);
    instruction.output(0, value);
    after_instruction(instruction.code, instruction.at)
}

/// The pc after the instruction at `at`. A call instruction of another opcode makes its call
/// as `CallGeneric` does, so the pc after a call is read from the code.
#[inline(always)]
unsafe fn after_instruction(code: *const i32, at: usize) -> usize {
    let Some(op) = op_of(*code.add(at) as u32) else {
        unreachable!("a call instruction has an opcode");
    };
    at + 1 + operand_count(op)
}

/// The arguments of the closure call `call` at `instruction`, each compiled to run as a promise
/// on the frame's slots where the compiler can, which the site compiles on its first call.
unsafe fn closure_arguments(
    state: &Activation<'_, '_>,
    instruction: Instruction,
    call: Sexp,
) -> R<Sexp> {
    let mut compiled = state.constants.element(instruction.operand(2) + 1);
    if compiled == unbound() {
        compiled = compile_promise_arguments(state.machine.function, call.cdr())?;
        // Promise code forced in a frame that has an environment runs for no function, so it
        // has no slot table to compile against; the site stays undecided until it runs on a
        // slot frame.
        if state.machine.function != nil() {
            (*state.heap).set_element(state.constants, instruction.operand(2) + 1, compiled);
        }
    }
    Ok(if compiled == nil() {
        call.cdr()
    } else {
        compiled
    })
}

/// How many times a call site may fail to plan, by meeting a closure with other formals or
/// body than its plan's or one it cannot plan for, before it stops planning and always takes
/// the generic call path: a site that calls a new closure on every call would otherwise pay
/// for matching each time.
const PLAN_MISSES: i32 = 8;

/// The direct-call plan of a call site for `function` (see [`eval::call_with_plan`]): which
/// argument fills each of its formals' slots, or `None` when calls of it here take the
/// generic path. A plan holds for every closure with the same formals and body, so the
/// closures one `function` expression makes on each run share it.
///
/// The site keeps its state in the feedback word of its instruction's first operand, which no
/// recording uses: `0` before its first call of a closure, a plan's id plus one, or minus one
/// plus the times it failed to plan. Most sites run once, so a site plans on its second call.
unsafe fn direct_plan(
    state: &Activation<'_, '_>,
    instruction: Instruction,
    function: Sexp,
    call: Sexp,
) -> R<Option<u32>> {
    let (formals, body) = (function.car(), function.cdr());
    if body.kind() != SexpType::Bytecode {
        return Ok(None);
    }
    let owner = state.constants;
    let word_at = instruction.at + 1;
    let Some(&word) = values::ints(owner.element(FEEDBACK)).get(word_at) else {
        return Ok(None);
    };
    let plans = &mut (*state.interp).bytecode.call_plans;
    let existing = (word > 0)
        .then(|| plans.owned(word as u32 - 1, owner, instruction.at))
        .flatten();
    let misses = match existing {
        Some(id) if plans.rows[id].formals == formals && plans.rows[id].body == body => {
            return Ok(Some(id as u32));
        }
        Some(id) => i32::from(plans.misses[id]),
        None if word == 0 => {
            values::ints_mut(owner.element(FEEDBACK))[word_at] = -1;
            return Ok(None);
        }
        None => (-word - 1).max(0),
    };
    if misses >= PLAN_MISSES {
        return Ok(None);
    }
    // A closure without formals has nothing to plan.
    let plan = eval::direct_sources(function, call)?.filter(|(_, sources)| !sources.is_empty());
    let plans = &mut (*state.interp).bytecode.call_plans;
    // Matching allocates, and a collection frees a plan whose closure died.
    let existing = existing.and_then(|id| plans.owned(id as u32, owner, instruction.at));
    // Replacing a plan made for another closure counts as a miss, as does a closure the site
    // cannot plan for.
    let misses = misses + i32::from(existing.is_some() || plan.is_none());
    let word = match (plan, existing) {
        (Some(plan), _) => {
            let arguments = if plan.1[..] == [eval::MATCH_PER_CALL] {
                0
            } else {
                length(call.cdr())
            };
            let id = plans.store(
                existing,
                owner,
                instruction.at,
                formals,
                body,
                arguments,
                &plan,
            );
            plans.misses[id] = misses as u8;
            values::ints_mut(owner.element(FEEDBACK))[word_at] = id as i32 + 1;
            return Ok(Some(id as u32));
        }
        (None, Some(id)) => {
            plans.misses[id] = misses as u8;
            return Ok(None);
        }
        (None, None) => -1 - misses,
    };
    values::ints_mut(owner.element(FEEDBACK))[word_at] = word;
    Ok(None)
}

/// The direct-call plans of the session's call sites ([`direct_plan`]), indexed by plan id: a
/// row of what every call of the site reads ([`PlanRow`]), and columns for the rest. A site
/// with no plan has no entry. The table holds its keys weakly: the code of the site, and the
/// formals and body of the closures the plan is for. A collection that finds one of them dead
/// frees the plan before the address can be reused, so no site matches a plan made for an
/// object that has since died.
#[derive(Default)]
pub struct CallPlans {
    rows: Vec<PlanRow>,
    /// How many times the site failed to plan ([`PLAN_MISSES`]).
    misses: Vec<u8>,
    /// How many words of [`CallPlans::pool`] the plan's range holds, so a site that plans again
    /// reuses it.
    capacities: Vec<u32>,
    /// The site's pin of a wrapper's lookups ([`WrapperPin`]): unbound until the site calls the
    /// wrapper, `NULL` for a closure that is not one. The one heap object a plan refers to
    /// strongly.
    pins: Vec<Sexp>,
    pool: Vec<i32>,
    free: Vec<u32>,
    /// Moves whenever a plan is stored or freed, so a site that read its plan before running
    /// its arguments knows the plan is unchanged after without checking its keys.
    generation: u32,
}

/// What a call reads of its site's plan, in one cache line.
#[derive(Clone, Copy)]
struct PlanRow {
    /// The constants of the code holding the site, or `NULL` for a free id.
    owner: Sexp,
    formals: Sexp,
    body: Sexp,
    /// The enclosure of the closure [`effects::effect_free`] last answered for at this site,
    /// with the binding epoch it answered at ([`PlanRow::effects_epoch`]) and the answer
    /// ([`PLAN_EFFECT_FREE`]): a closure with the same enclosure called again under the same
    /// epoch gets the same answer without reading the proof
    /// ([`effects::effect_free_for_epoch`]). `NULL` when no answer is kept. Any collection
    /// drops it, because the row does not keep the enclosure alive and its address can be
    /// reused.
    effects_env: Sexp,
    effects_epoch: u64,
    /// The position of the site's call instruction in that code.
    site: u32,
    /// Where the plan's sources start in [`CallPlans::pool`]. The kinds of the site's
    /// arguments ([`prepare_plan`]) follow them.
    sources_at: u32,
    sources_len: u32,
    /// How many arguments the site writes.
    arguments: u16,
    shape: eval::CallShape,
    /// How many `for` loops the body has.
    loops: u16,
    flags: u8,
}

/// The [`PlanRow::flags`] bit set once the kinds of the site's arguments are worked out.
const PLAN_PREPARED: u8 = 1;
/// The [`PlanRow::flags`] bit set when [`call_fast`] may take the site's calls.
const PLAN_FAST: u8 = 2;
/// The [`PlanRow::flags`] bit holding the answer [`PlanRow::effects_env`] is kept for.
const PLAN_EFFECT_FREE: u8 = 4;

/// The kind of an argument a site passes as it is written: a constant, or the empty argument.
const ARGUMENT_VALUE: i32 = -1;
/// The kind of an argument that names a variable the calling frame keeps in no slot. A
/// variable in a slot has the slot's index as its kind.
const ARGUMENT_OUTSIDE: i32 = -2;
/// The kind of an argument that is code compiled to run on the calling frame's slots.
const ARGUMENT_CODE: i32 = -3;

impl CallPlans {
    /// The plan `id` when it belongs to the site at `at` in the code whose constants are
    /// `owner`. A site's word can name an id that was freed and reused, or come from a session
    /// that saved the code.
    fn owned(&self, id: u32, owner: Sexp, at: usize) -> Option<usize> {
        let id = id as usize;
        self.rows
            .get(id)
            .is_some_and(|row| row.owner == owner && row.site as usize == at)
            .then_some(id)
    }

    pub fn sources(&self, id: u32) -> &[i32] {
        let row = &self.rows[id as usize];
        let at = row.sources_at as usize;
        &self.pool[at..at + row.sources_len as usize]
    }

    /// The plan `id` with its shape and sources, when it is still the site's plan for closures
    /// with `function`'s formals and body, as it was when the table's generation was
    /// `generation`. Running the arguments can run R code that replans the site, or frees the
    /// plan and reuses its id.
    #[inline]
    unsafe fn current(
        &self,
        id: u32,
        generation: u32,
        owner: Sexp,
        at: usize,
        function: Sexp,
    ) -> Option<(eval::CallShape, &[i32])> {
        let id = if generation == self.generation {
            id as usize
        } else {
            let id = self.owned(id, owner, at)?;
            if self.rows[id].formals != function.car() || self.rows[id].body != function.cdr() {
                return None;
            }
            id
        };
        Some((self.rows[id].shape, self.sources(id as u32)))
    }

    /// Stores a plan for the site, which writes `arguments` arguments, in `existing`'s place
    /// when it has one.
    #[allow(clippy::too_many_arguments)]
    unsafe fn store(
        &mut self,
        existing: Option<usize>,
        owner: Sexp,
        at: usize,
        formals: Sexp,
        body: Sexp,
        arguments: usize,
        (shape, sources): &eval::CallPlan,
    ) -> usize {
        self.generation = self.generation.wrapping_add(1);
        let id = match existing.or_else(|| self.free.pop().map(|id| id as usize)) {
            Some(id) => id,
            None => {
                self.rows.push(PlanRow {
                    owner: nil(),
                    formals: nil(),
                    body: nil(),
                    effects_env: nil(),
                    effects_epoch: 0,
                    site: 0,
                    sources_at: 0,
                    sources_len: 0,
                    arguments: 0,
                    shape: eval::CallShape::default(),
                    loops: 0,
                    flags: 0,
                });
                self.misses.push(0);
                self.capacities.push(0);
                self.pins.push(nil());
                self.rows.len() - 1
            }
        };
        self.pins[id] = unbound();
        let length = sources.len() + arguments;
        let mut sources_at = self.rows[id].sources_at;
        if length > self.capacities[id] as usize {
            sources_at = self.pool.len() as u32;
            self.capacities[id] = length as u32;
            self.pool.resize(self.pool.len() + length, 0);
        }
        let start = sources_at as usize;
        self.pool[start..start + sources.len()].copy_from_slice(sources);
        self.rows[id] = PlanRow {
            owner,
            formals,
            body,
            effects_env: nil(),
            effects_epoch: 0,
            site: at as u32,
            sources_at,
            sources_len: sources.len() as u32,
            // A site with more arguments, or a body with more loops, than the row counts never
            // prepares ([`prepare_plan`]).
            arguments: u16::try_from(arguments).unwrap_or(u16::MAX),
            shape: *shape,
            loops: u16::try_from(loop_count(body)).unwrap_or(u16::MAX),
            flags: 0,
        };
        id
    }

    unsafe fn trace(&self, marker: &mut Marker<'_>) {
        for &pin in &self.pins {
            marker.mark(pin);
        }
    }

    /// Frees every plan with a dead key. A major collection also drops every pin: a pin holds
    /// the closure's environment, which can reach the very code and body that key the plan,
    /// and would otherwise keep the plan alive forever. A site whose pin is dropped decides
    /// again whether its callee is a wrapper, so its calls leave the fast path until it has.
    pub unsafe fn clear_dead(&mut self, liveness: &Liveness<'_>) {
        let live = |object: Sexp| object == nil() || liveness.is_live(object);
        for id in 0..self.rows.len() {
            let row = &mut self.rows[id];
            if row.owner == nil() {
                continue;
            }
            row.effects_env = nil();
            if !live(row.owner) || !live(row.formals) || !live(row.body) {
                row.owner = nil();
                row.formals = nil();
                row.body = nil();
                row.flags = 0;
                self.pins[id] = nil();
                self.free.push(id as u32);
                self.generation = self.generation.wrapping_add(1);
            } else if liveness.is_major() {
                row.flags = 0;
                self.pins[id] = unbound();
            }
        }
    }
}

/// Works out, once the site at `at` knows its callee is no wrapper, how each of the site's
/// `arguments` binds ([`ARGUMENT_VALUE`] and the kinds beside it), and whether [`call_fast`]
/// may take the site's calls: every argument is a constant, the empty argument, a variable or
/// code compiled against the calling frame's slots. `...`, `..N` and code that needs the
/// frame's environment leave the site to the generic path.
unsafe fn prepare_plan(state: &Activation<'_, '_>, id: u32, arguments: Sexp) {
    let mut kinds: SmallVec<[i32; 8]> = SmallVec::new();
    let mut fast = true;
    let mut cell = arguments;
    while cell.kind() == SexpType::Pairlist {
        let expression = cell.car();
        kinds.push(match expression.kind() {
            _ if expression == missing_arg() => ARGUMENT_VALUE,
            SexpType::Symbol
                if expression == symbols().dots || eval::dot_dot_index(expression).is_some() =>
            {
                fast = false;
                break;
            }
            SexpType::Symbol => {
                slot_index(state, expression).map_or(ARGUMENT_OUTSIDE, |slot| slot as i32)
            }
            SexpType::Bytecode if expression.cdr().element(SLOTS) != nil() => ARGUMENT_CODE,
            SexpType::Bytecode | SexpType::Language | SexpType::Promise => {
                fast = false;
                break;
            }
            _ => ARGUMENT_VALUE,
        });
        cell = cell.cdr();
    }
    let plans = &mut (*state.interp).bytecode.call_plans;
    let row = &mut plans.rows[id as usize];
    row.flags |= PLAN_PREPARED;
    if !fast || kinds.len() != row.arguments as usize || row.loops == u16::MAX {
        return;
    }
    let start = (row.sources_at + row.sources_len) as usize;
    row.flags |= PLAN_FAST;
    plans.pool[start..start + kinds.len()].copy_from_slice(&kinds);
}

/// Runs a planned call of `function` with [`eval::run_wrapper`] when the closure is a wrapper,
/// a body that is one call of a builtin whose arguments are formals, literals and variables
/// bound outside any function frame, and each argument of the call reads without running R
/// code. `None` leaves the call to the generic path, with nothing changed.
///
/// The site pins what the body's lookups found in its plan ([`WrapperPin`]),
/// and a pin holds while the stamps of its symbols do: a binding of one appearing or going
/// away anywhere outside a function frame, or any enclosure changing, moves a stamp, while a
/// value assigned to a pinned binding is read from its cell on every call.
unsafe fn call_wrapper(
    state: &mut Activation<'_, '_>,
    function: Sexp,
    call: Sexp,
    plan: u32,
    sources: &[i32],
) -> R<Option<Sexp>> {
    let interp = &*state.interp;
    if function.has_gp(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP | gp::CLOSURE_TRACE)
        || interp.debugger.is_some()
        || !interp.object_systems
        || crate::moves::tracking()
    {
        return Ok(None);
    }
    let mut pin = interp.bytecode.call_plans.pins[plan as usize];
    if pin == nil() {
        return Ok(None);
    }
    if pin == unbound() || !pin_holds(pin, function) {
        pin = pin_wrapper(function)?;
        (&mut (*state.interp).bytecode.call_plans.pins)[plan as usize] = pin;
        if pin == nil() {
            return Ok(None);
        }
    }
    let Some(builtin) = cell_value(pin.element(WrapperPin::CELLS).element(0)) else {
        return Ok(None);
    };
    if builtin.kind() != SexpType::Builtin {
        return Ok(None);
    }
    let mut supplied: SmallVec<[Arg; 4]> = SmallVec::new();
    let mut cell = call.cdr();
    while cell.kind() == SexpType::Pairlist {
        let Some(value) = wrapper_argument(state, cell.car())? else {
            return Ok(None);
        };
        supplied.push(Arg {
            tag: cell.tag(),
            value,
        });
        cell = cell.cdr();
    }
    if sources.iter().filter(|&&source| source >= 0).count() != supplied.len() {
        return Ok(None);
    }
    let mut formal_values: SmallVec<[Sexp; 8]> = SmallVec::new();
    let mut missing = 0u64;
    let mut formal = function.car();
    for (index, &source) in sources.iter().enumerate() {
        let value = match supplied.get(source as usize) {
            Some(arg) if source >= 0 => arg.value,
            _ => {
                let default = formal.car();
                if default == missing_arg() {
                    return Ok(None);
                }
                missing |= 1 << index;
                default
            }
        };
        formal_values.push(value);
        formal = formal.cdr();
    }
    let kinds = values::ints(pin.element(WrapperPin::KINDS));
    let payload = pin.element(WrapperPin::PAYLOAD);
    let body = wrapper_call(source_body(function));
    let mut builtin_args: SmallVec<[Arg; 4]> = SmallVec::new();
    let mut cell = body.cdr();
    for (index, &kind) in kinds.iter().enumerate() {
        let value = match kind {
            WrapperPin::LITERAL => payload.element(index),
            WrapperPin::FREE => match cell_value(payload.element(index)) {
                Some(value) => value,
                None => return Ok(None),
            },
            formal => match formal_values.get(formal as usize) {
                Some(&value) => value,
                None => return Ok(None),
            },
        };
        builtin_args.push(Arg {
            tag: cell.tag(),
            value,
        });
        cell = cell.cdr();
    }
    let count = length(function.cdr().cdr().element(SLOTS).element(1));
    let header = (*state.interp).bytecode.registers.len();
    reserve_registers(header + 1 + count)?;
    let window = register_address(header);
    for index in 0..count {
        let value = formal_values.get(index).copied().unwrap_or_else(unbound);
        value.increment_refcount();
        *window.add(1 + index) = value;
    }
    let sys_parent = if state.in_slots() {
        nil()
    } else {
        state.env(Reify::Site("wrapper call"))?
    };
    activity::count(Counter::WrappersInlined);
    eval::run_inlined(
        call,
        function,
        header as u32,
        missing,
        sys_parent,
        &supplied,
        |frame| {
            let definition = eval::builtin_def(builtin)?;
            let state = crate::interp();
            let owner = state.slot_owner;
            state.slot_owner = frame;
            let result = call_builtin(definition, body, builtin, &builtin_args, nil());
            crate::interp().slot_owner = owner;
            result
        },
    )
    .map(Some)
}

/// Runs a call of base R's `do.call(what, args)` from the call site, without the closure's
/// body: the frame `do.call` would have is pushed with its formals' values
/// ([`eval::run_inlined`]), and the target is called directly with the list's elements as
/// its arguments, from the caller of `do.call` as the body's `parent.frame()` default would
/// have it. The body's own steps (forcing `args` for `is.list`, the `parent.frame()` promise,
/// the `.Internal`, building the call and evaluating it in an environment the caller then
/// needs) are what makes `do.call` cost several ordinary calls.
///
/// `None`, having run nothing, when the call is anything else: `quote` or `envir` given, an
/// argument that is not a value reading runs no R code for, `args` not a list, an element
/// that `do.call` would evaluate (a name or a call), or a target found by name that the
/// caller's own slots may bind. A builtin target with S3 dispatch runs only from a frame with
/// an environment, because dispatch reads the caller's environment.
unsafe fn call_do_call(
    state: &mut Activation<'_, '_>,
    function: Sexp,
    call: Sexp,
    sources: &[i32],
) -> R<Option<Sexp>> {
    let interp = &*state.interp;
    if function != interp.do_call
        || function.has_gp(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP | gp::CLOSURE_TRACE)
        || interp.debugger.is_some()
        || !interp.object_systems
        || crate::moves::tracking()
    {
        return Ok(None);
    }
    // The formals are `what`, `args`, `quote` and `envir`; the plan maps each to the argument
    // it takes, or a negative source when it is missing.
    let &[what_source, args_source, quote_source, envir_source] = sources else {
        return Ok(None);
    };
    if what_source < 0 || args_source < 0 || quote_source >= 0 || envir_source >= 0 {
        return Ok(None);
    }
    let mut supplied: SmallVec<[Arg; 2]> = SmallVec::new();
    let mut cell = call.cdr();
    while cell.kind() == SexpType::Pairlist {
        let Some(value) = wrapper_argument(state, cell.car())? else {
            return Ok(None);
        };
        supplied.push(Arg {
            tag: cell.tag(),
            value,
        });
        cell = cell.cdr();
    }
    let (Some(what), Some(list)) = (
        supplied.get(what_source as usize),
        supplied.get(args_source as usize),
    ) else {
        return Ok(None);
    };
    let (what, list) = (what.value, list.value);
    if list.kind() != SexpType::List && list != nil() {
        return Ok(None);
    }
    let in_slots = state.in_slots();
    let (target, head) = match what.kind() {
        SexpType::Closure | SexpType::Builtin => (what, what),
        SexpType::String if length(what) == 1 && what.element(0) != values::na_string() => {
            let symbol = values::install_charsxp(what.element(0))?;
            let start = if in_slots {
                if slot_index(state, symbol).is_some() {
                    return Ok(None);
                }
                state.machine.function.tag()
            } else {
                state.env(Reify::Site("do.call"))?
            };
            match effects::resolve_function(symbol, start) {
                Some(found) => (found.function, symbol),
                None => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    match target.kind() {
        SexpType::Closure => {}
        SexpType::Builtin if !in_slots || eval::builtin_def(target)?.generic == Generic::None => {}
        _ => return Ok(None),
    }
    let count = length(list);
    let names = values::names(list);
    let mut args = eval::Args::new();
    let mut items: SmallVec<[(Sexp, Sexp); 8]> = SmallVec::new();
    items.push((nil(), head));
    for index in 0..count {
        let value = list.element(index);
        if matches!(
            value.kind(),
            SexpType::Symbol | SexpType::Language | SexpType::Promise | SexpType::Bytecode
        ) {
            return Ok(None);
        }
        // As the internal does: an empty name tags nothing, and `NA` tags its argument `NA`.
        let tag = if names == nil() {
            nil()
        } else {
            let name = names.element(index);
            if name.bytes().is_empty() {
                nil()
            } else if name == values::na_string() {
                values::install("NA")?
            } else {
                values::install_charsxp(name)?
            }
        };
        items.push((tag, value));
        args.push(Arg { tag, value });
    }
    let target_call = values::pairlist(&items, SexpType::Language)?;
    let heap = crate::heap();
    heap.root(target_call);
    // The frame's slots hold `what` and `args`; `quote` and `envir` are missing and hold their
    // defaults, which a reify of the frame turns into promises.
    let slot_count = length(function.cdr().cdr().element(SLOTS).element(1));
    let header = (*state.interp).bytecode.registers.len();
    reserve_registers(header + 1 + slot_count)?;
    let window = register_address(header);
    let mut formal = function.car();
    for index in 0..slot_count {
        let value = match index {
            0 => what,
            1 => list,
            _ if formal != nil() => formal.car(),
            _ => unbound(),
        };
        value.increment_refcount();
        *window.add(1 + index) = value;
        if formal != nil() {
            formal = formal.cdr();
        }
    }
    let sys_parent = if in_slots {
        nil()
    } else {
        state.env(Reify::Site("do.call"))?
    };
    activity::count(Counter::DoCallsInlined);
    eval::run_inlined(
        call,
        function,
        header as u32,
        0b1100,
        sys_parent,
        &supplied,
        |_| eval::apply(target_call, target, &args, sys_parent),
    )
    .map(Some)
}

/// The call a wrapper's body makes: the body itself, or the one call inside `{ }`.
unsafe fn wrapper_call(body: Sexp) -> Sexp {
    if body.kind() == SexpType::Language
        && body.car() == symbols().brace
        && body.cdr().kind() == SexpType::Pairlist
        && body.cdr().cdr() == nil()
        && body.cdr().car().kind() == SexpType::Language
    {
        body.cdr().car()
    } else {
        body
    }
}

/// The layout of a call site's pin of a wrapper's lookups (see [`call_wrapper`]), a list.
struct WrapperPin;

impl WrapperPin {
    /// The closure environment the lookups started from.
    const ENCLOSURE: usize = 0;
    /// Per argument of the body's builtin call, the formal it passes (its index), or
    /// [`WrapperPin::LITERAL`] or [`WrapperPin::FREE`].
    const KINDS: usize = 1;
    /// Per argument, the literal, the cell of the free variable, or `NULL` for a formal.
    const PAYLOAD: usize = 2;
    /// The pinned symbols, the builtin's name first.
    const SYMBOLS: usize = 3;
    /// The cell each pinned symbol resolved to.
    const CELLS: usize = 4;
    /// The stamp of each pinned symbol when it was resolved, as the bits of a double.
    const STAMPS: usize = 5;
    const LENGTH: usize = 6;
    const LITERAL: i32 = -1;
    const FREE: i32 = -2;
}

/// Whether `pin` still describes `function`'s lookups: the closure has the same environment
/// and no pinned symbol's stamp moved.
#[inline]
unsafe fn pin_holds(pin: Sexp, function: Sexp) -> bool {
    if pin.element(WrapperPin::ENCLOSURE) != function.tag() {
        return false;
    }
    let heap = crate::heap();
    let symbols = pin.element(WrapperPin::SYMBOLS);
    let stamps = values::reals(pin.element(WrapperPin::STAMPS));
    stamps
        .iter()
        .enumerate()
        .all(|(index, stamp)| stamp.to_bits() == heap.lookup_stamp(symbols.element(index)))
}

/// The pin of `function`'s lookups when the closure is a wrapper, or `NULL`. A wrapper's body
/// is one call whose function is a name bound outside any function frame and whose arguments
/// are formals, literals, or other such names, and it has no `...` formal.
unsafe fn pin_wrapper(function: Sexp) -> R<Sexp> {
    let source = source_body(function);
    let body = wrapper_call(source);
    let enclosure = function.tag();
    let constants = function.cdr().cdr();
    if body.kind() != SexpType::Language
        || body.car().kind() != SexpType::Symbol
        || constants.element(SLOTS) == nil()
    {
        return Ok(nil());
    }
    let mut formals: SmallVec<[Sexp; 8]> = SmallVec::new();
    let mut formal = function.car();
    while formal.kind() == SexpType::Pairlist {
        if formal.tag() == symbols().dots || formals.len() >= MAX_SLOT_FORMALS {
            return Ok(nil());
        }
        let default = formal.car();
        if !(default == missing_arg() || default == nil() || default.kind().is_atomic()) {
            return Ok(nil());
        }
        formals.push(formal.tag());
        formal = formal.cdr();
    }
    let mut pinned: SmallVec<[(Sexp, Sexp); 4]> = SmallVec::new();
    let head = body.car();
    if formals.contains(&head) {
        return Ok(nil());
    }
    // Only a builtin head runs without a body, so any other head (`{` with several statements,
    // a closure) settles the site on the generic path for good instead of pinning lookups that
    // every call would check and then discard.
    let Some(cell) = resolve(head, enclosure, false)? else {
        return Ok(nil());
    };
    if cell_value(cell).is_none_or(|value| value.kind() != SexpType::Builtin) {
        return Ok(nil());
    }
    pinned.push((head, cell));
    // `{ f(x) }` runs as `f(x)` only while `{` is base R's.
    if body != source {
        let brace = symbols().brace;
        let Some(cell) = resolve(brace, enclosure, false)? else {
            return Ok(nil());
        };
        match cell_value(cell) {
            Some(value)
                if value.kind() == SexpType::Special && values::primitive_name(value) == "{" => {}
            _ => return Ok(nil()),
        }
        pinned.push((brace, cell));
    }
    let mut kinds: SmallVec<[i32; 8]> = SmallVec::new();
    let mut payload: SmallVec<[Sexp; 8]> = SmallVec::new();
    let mut argument = body.cdr();
    while argument.kind() == SexpType::Pairlist {
        let expression = argument.car();
        match expression.kind() {
            SexpType::Symbol => {
                if expression == missing_arg() || expression == symbols().dots {
                    return Ok(nil());
                }
                if let Some(index) = formals.iter().position(|&name| name == expression) {
                    kinds.push(index as i32);
                    payload.push(nil());
                } else {
                    let Some(cell) = resolve(expression, enclosure, false)? else {
                        return Ok(nil());
                    };
                    kinds.push(WrapperPin::FREE);
                    payload.push(cell);
                    pinned.push((expression, cell));
                }
            }
            SexpType::Language | SexpType::Promise | SexpType::Bytecode => return Ok(nil()),
            _ => {
                kinds.push(WrapperPin::LITERAL);
                payload.push(expression);
            }
        }
        argument = argument.cdr();
    }
    let heap = crate::heap();
    let pin = values::alloc(SexpType::List, WrapperPin::LENGTH)?;
    heap.root(pin);
    heap.set_element(pin, WrapperPin::ENCLOSURE, enclosure);
    heap.set_element(pin, WrapperPin::KINDS, values::integer_vector(&kinds)?);
    let list = values::alloc(SexpType::List, payload.len())?;
    heap.set_element(pin, WrapperPin::PAYLOAD, list);
    for (index, &value) in payload.iter().enumerate() {
        heap.set_element(list, index, value);
    }
    let names = values::alloc(SexpType::List, pinned.len())?;
    heap.set_element(pin, WrapperPin::SYMBOLS, names);
    let cells = values::alloc(SexpType::List, pinned.len())?;
    heap.set_element(pin, WrapperPin::CELLS, cells);
    let stamps = values::alloc(SexpType::Real, pinned.len())?;
    heap.set_element(pin, WrapperPin::STAMPS, stamps);
    let stamp_values = values::reals_mut(stamps);
    for (index, &(symbol, cell)) in pinned.iter().enumerate() {
        heap.set_element(names, index, symbol);
        heap.set_element(cells, index, cell);
        stamp_values[index] = f64::from_bits(heap.lookup_stamp(symbol));
    }
    Ok(pin)
}

/// The value a pinned cell holds when reading it runs no R code.
#[inline]
unsafe fn cell_value(cell: Sexp) -> Option<Sexp> {
    let value = cell.cdr();
    if value.kind() != SexpType::Promise {
        return (value != unbound() && value != missing_arg()).then_some(value);
    }
    (value.car() != unbound()).then(|| value.car())
}

/// The value of a call's argument `expression` when reading it runs no R code: a literal, or a
/// variable bound to a value or to a promise that reading settles without running code.
#[inline]
unsafe fn wrapper_argument(state: &Activation<'_, '_>, expression: Sexp) -> R<Option<Sexp>> {
    let value = match expression.kind() {
        SexpType::Symbol => {
            if expression == missing_arg() || expression == symbols().dots {
                return Ok(None);
            }
            let found = if state.in_slots() {
                slot_eager_argument(state, expression)
            } else {
                effects::eager_argument(expression, state.env(Reify::Site("wrapper argument"))?)
            };
            match found {
                Some(value) => value,
                None => return Ok(None),
            }
        }
        SexpType::Language | SexpType::Promise | SexpType::Bytecode => return Ok(None),
        _ => return Ok(Some(expression)),
    };
    if value.kind() != SexpType::Promise {
        return Ok((value != missing_arg() && value != unbound()).then_some(value));
    }
    if value.car() != unbound() {
        return Ok(Some(value.car()));
    }
    eval::settled_without_code(value)
}

/// The slot of `symbol` in a slot frame, found by name for instructions that carry no lookup
/// cache. Slot tables are short, so the scan costs less than a hash lookup.
/// Fills the fourth element of a code object's [`SLOTS`] table: for each constant that a
/// `ForStep` or `SubAssign` names as its variable, that variable's slot, so those handlers find
/// it with one load ([`slot_at`]); -1 elsewhere. The map ends at the last such constant, and code
/// with neither op has none, because most code has no loop or element assignment and a map over
/// every constant of every function costs megabytes. It is derived from the code, the names and
/// the constants, so the code image stores neither it nor anything it could disagree with.
pub(crate) unsafe fn index_slots(words: Sexp, constants: Sexp) -> R<()> {
    let table = constants.element(SLOTS);
    if table == nil() {
        return Ok(());
    }
    let names = table.element(1);
    let code = values::ints(words);
    let mut variables: SmallVec<[(usize, i32); 8]> = SmallVec::new();
    let mut pc = 0;
    while let Some(&word) = code.get(pc) {
        let op =
            op_of(word as u32).ok_or_else(|| error("the code holds an invalid instruction"))?;
        let variable = match op {
            Op::ForStep => code.get(pc + 3),
            Op::SubAssign => code.get(pc + 2),
            _ => None,
        };
        if let Some(&constant) = variable
            && let constant = constant as usize
            && constant < length(constants)
            && let symbol = constants.element(constant)
            && symbol.kind() == SexpType::Symbol
            && let Some(slot) = (0..length(names)).find(|&slot| names.element(slot) == symbol)
        {
            variables.push((constant, slot as i32));
        }
        pc += 1 + operand_count(op);
    }
    let Some(last) = variables.iter().map(|&(constant, _)| constant).max() else {
        return Ok(());
    };
    let by_constant = values::alloc(SexpType::Integer, last + 1)?;
    let map = values::ints_mut(by_constant);
    map.fill(-1);
    for (constant, slot) in variables {
        map[constant] = slot;
    }
    crate::heap().set_element(table, 3, by_constant);
    Ok(())
}

/// The frame slot of the symbol at constant `constant`, as [`slot_named`] finds it.
#[inline(always)]
unsafe fn slot_at(state: &Activation<'_, '_>, constant: usize) -> Option<*mut Sexp> {
    let by_constant = state.constants.element(SLOTS).element(3);
    if by_constant == nil() {
        return None;
    }
    let slot = *values::ints(by_constant).get(constant)?;
    (slot >= 0).then(|| state.header.add(1 + slot as usize))
}

unsafe fn slot_named(state: &Activation<'_, '_>, symbol: Sexp) -> Option<*mut Sexp> {
    slot_index(state, symbol).map(|position| state.header.add(1 + position))
}

/// The position of `symbol` in the slot frame's table of slot names. The table is a list the
/// compiler made, never ALTREP, so the scan reads its storage directly.
unsafe fn slot_index(state: &Activation<'_, '_>, symbol: Sexp) -> Option<usize> {
    let names = state.constants.element(SLOTS).element(1);
    names
        .slice::<Sexp>()
        .iter()
        .position(|&name| name == symbol)
}

/// `substitute(x)` of a single name in a slot frame, read from the slots without giving the
/// frame an environment: package code calls `deparse(substitute(x))` for labels and messages
/// on hot paths. It answers as [`eval::substitute`] does on the frame's environment: a
/// promise's code, a local's value, or the name itself when the frame does not bind it.
unsafe fn slot_substitute(
    state: &Activation<'_, '_>,
    function: Sexp,
    call: Sexp,
) -> R<Option<Sexp>> {
    let arguments = call.cdr();
    if function.kind() != SexpType::Special
        || arguments == nil()
        || arguments.cdr() != nil()
        || arguments.tag() != nil()
        || eval::builtin_def(function)?.name != "substitute"
    {
        return Ok(None);
    }
    let symbol = arguments.car();
    if symbol.kind() != SexpType::Symbol || symbol == symbols().dots {
        return Ok(None);
    }
    let Some(slot) = slot_named(state, symbol) else {
        interp().visible = true;
        return Ok(Some(symbol));
    };
    let value = *slot;
    let result = if value == unbound() {
        symbol
    } else if value.kind() == SexpType::Promise {
        let mut code = value.cdr();
        while code.kind() == SexpType::Promise {
            code = code.cdr();
        }
        code
    } else {
        value
    };
    interp().visible = true;
    Ok(Some(result))
}

/// A call of a special from a slot frame that the slots answer, so the frame keeps going without
/// an environment. Base wrappers call these on hot paths: `.Internal(f(x, ...))` (the internals
/// the compiler leaves to the special because they dispatch or take `...`), a generic's
/// `UseMethod("name")`, and `quote()` of an empty argument.
///
/// A call answered here never reaches the evaluator's builtin call, so the activity record counts
/// and times it here: otherwise a profile would show every S3 dispatch from a slot frame with no
/// `UseMethod` call.
unsafe fn slot_special_call(
    state: &Activation<'_, '_>,
    instruction: Instruction,
    function: Sexp,
    call: Sexp,
) -> R<Option<Sexp>> {
    if call.cdr() == nil() {
        return Ok(None);
    }
    let definition = eval::builtin_def(function)?;
    if !activity::enabled() {
        return slot_special_answer(state, instruction, definition.name, call);
    }
    let index = eval::builtin_index(definition);
    let _activity = activity::enter(Activity::Builtin, index as u32);
    let answer = slot_special_answer(state, instruction, definition.name, call)?;
    if answer.is_some() {
        activity::count(Counter::BuiltinCalls);
        activity::count_builtin(index);
    }
    Ok(answer)
}

unsafe fn slot_special_answer(
    state: &Activation<'_, '_>,
    instruction: Instruction,
    name: &str,
    call: Sexp,
) -> R<Option<Sexp>> {
    let arguments = call.cdr();
    let argument = arguments.car();
    if name == "on.exit" {
        return slot_on_exit(state, instruction, call);
    }
    if arguments.cdr() != nil() {
        let rest = arguments.cdr();
        if rest.cdr() != nil() || arguments.tag() != nil() || rest.tag() != nil() {
            return Ok(None);
        }
        return match name {
            // `UseMethod("generic", object)` dispatches on a local's value.
            "UseMethod" if argument.kind() == SexpType::String && length(argument) == 1 => {
                match slot_operand(state, rest.car())? {
                    Some(object) => slot_use_method(state, instruction, argument, Some(object)),
                    None => Ok(None),
                }
            }
            // `name <<- value` assigns from the enclosure outwards, so only the value reads the
            // frame.
            "<<-"
                if argument.kind() == SexpType::Symbol
                    && eval::dot_dot_index(argument).is_none() =>
            {
                let Some(value) = slot_operand(state, rest.car())? else {
                    return Ok(None);
                };
                eval::set_var(argument, value, state.machine.function.tag())?;
                interp().visible = false;
                Ok(Some(value))
            }
            "switch" if rest.car() == symbols().dots => {
                slot_switch(state, instruction, call, argument)
            }
            _ => Ok(None),
        };
    }
    match name {
        ".Internal" if arguments.tag() == nil() => {
            if argument.kind() != SexpType::Language || argument.car().kind() != SexpType::Symbol {
                return Ok(None);
            }
            match crate::builtins::internal_function(argument.car()) {
                Some(internal) if internal.kind() == SexpType::Builtin => {
                    slot_builtin_call(state, argument, internal, None)
                }
                _ => Ok(None),
            }
        }
        "UseMethod"
            if arguments.tag() == nil()
                && argument.kind() == SexpType::String
                && length(argument) == 1 =>
        {
            slot_use_method(state, instruction, argument, None)
        }
        "quote" if arguments.tag() == nil() || values::symbol_name(arguments.tag()) == "expr" => {
            interp().visible = true;
            Ok(Some(argument))
        }
        _ => Ok(None),
    }
}

/// The call site's arguments compiled as promise code for the running function's slots, which
/// a special called from a slot frame runs there. The site's promise-code entry serves this:
/// it is otherwise read only when the site calls a closure, and `compile_promise_arguments`
/// builds the same list for that.
unsafe fn site_promise_code(
    state: &Activation<'_, '_>,
    instruction: Instruction,
    call: Sexp,
) -> R<Sexp> {
    let mut compiled = state.constants.element(instruction.operand(2) + 1);
    if compiled == unbound() {
        compiled = compile_promise_arguments(state.machine.function, call.cdr())?;
        (*state.heap).set_element(state.constants, instruction.operand(2) + 1, compiled);
    }
    Ok(compiled)
}

/// `on.exit(expr, add, after)` from a slot frame, with `add` and `after` given as logical
/// constants or left out. The frame's exit list keeps the expression compiled for its slots
/// when it compiles, so running it at exit needs no environment either.
unsafe fn slot_on_exit(
    state: &Activation<'_, '_>,
    instruction: Instruction,
    call: Sexp,
) -> R<Option<Sexp>> {
    if state.machine.function == nil() {
        return Ok(None);
    }
    const NAMES: [&str; 3] = ["expr", "add", "after"];
    let mut expression = missing_arg();
    let (mut add, mut after) = (false, true);
    for (position, (tag, value)) in values::pairlist_items(call.cdr()).into_iter().enumerate() {
        if position >= NAMES.len() || (tag != nil() && symbol_name(tag) != NAMES[position]) {
            return Ok(None);
        }
        if position == 0 {
            expression = value;
            continue;
        }
        if value == missing_arg() {
            continue;
        }
        if value.kind() != SexpType::Logical || length(value) != 1 {
            return Ok(None);
        }
        let flag = values::as_logical_scalar(value);
        if flag == NA_LOGICAL {
            return Ok(None);
        }
        if position == 1 {
            add = flag == 1;
        } else {
            after = flag != 0;
        }
    }
    let registered = match site_promise_code(state, instruction, call)?.car() {
        code if code.kind() == SexpType::Bytecode => code,
        _ => expression,
    };
    let owner = (*state.interp).slot_owner as usize;
    let frame = &mut interp().frames[owner];
    if !add {
        frame.on_exit.clear();
    }
    if expression != missing_arg() && expression != nil() {
        if after {
            frame.on_exit.push(registered);
        } else {
            frame.on_exit.insert(0, registered);
        }
    }
    interp().visible = false;
    Ok(Some(nil()))
}

/// `switch(selector, ...)` from a slot frame, choosing among the alternatives its `...` holds.
/// They are its caller's promises, so only the selector reads the frame, and it runs as promise
/// code on the slots, compiled once per call site.
unsafe fn slot_switch(
    state: &Activation<'_, '_>,
    instruction: Instruction,
    call: Sexp,
    selector: Sexp,
) -> R<Option<Sexp>> {
    let function = state.machine.function;
    let Some(dots) = slot_named(state, symbols().dots) else {
        return Ok(None);
    };
    if function == nil() || !forwardable_dots(*dots) {
        return Ok(None);
    }
    let value = if selector.kind() == SexpType::Language {
        let code = site_promise_code(state, instruction, call)?.car();
        if code.kind() != SexpType::Bytecode {
            return Ok(None);
        }
        let owner = (*state.interp).slot_owner as usize;
        run_promise_on_slots(code, function, owner, state.machine.header)?
    } else {
        match slot_operand(state, selector)? {
            Some(value) => value,
            None => return Ok(None),
        }
    };
    let dots = *dots;
    let alternatives: Vec<Arg> = if dots.kind() == SexpType::Dots {
        values::pairlist_items(dots)
            .into_iter()
            .map(|(tag, value)| Arg { tag, value })
            .collect()
    } else {
        Vec::new()
    };
    match eval::switch_alternative(value, call, &alternatives)? {
        Some(chosen) if chosen.kind() == SexpType::Promise => eval::force_promise(chosen).map(Some),
        // The special evaluates the alternative it chose in the frame, as `do_switch` does.
        Some(chosen) if matches!(chosen.kind(), SexpType::Language | SexpType::Symbol) => {
            eval::eval(chosen, state.env(Reify::Site("switch alternative"))?).map(Some)
        }
        Some(chosen) => {
            interp().visible = true;
            Ok(Some(chosen))
        }
        None => {
            interp().visible = false;
            Ok(Some(nil()))
        }
    }
}

/// The value of `expression`, an argument of a special called from a slot frame, when the slots
/// answer it without an environment: a constant, or a local holding a value or a promise.
/// `None` also when forcing the promise gave the frame its environment; the generic path then
/// evaluates the argument again and finds the promise forced already.
unsafe fn slot_operand(state: &Activation<'_, '_>, expression: Sexp) -> R<Option<Sexp>> {
    let value = match expression.kind() {
        SexpType::Symbol if eval::dot_dot_index(expression).is_none() => {
            match slot_named(state, expression) {
                Some(slot) => *slot,
                None => return Ok(None),
            }
        }
        SexpType::Symbol
        | SexpType::Language
        | SexpType::Promise
        | SexpType::Bytecode
        | SexpType::Dots => return Ok(None),
        _ if expression == missing_arg() => return Ok(None),
        _ => return Ok(Some(expression)),
    };
    if value == missing_arg() || value == unbound() || value.kind() == SexpType::Dots {
        return Ok(None);
    }
    if value.kind() != SexpType::Promise {
        return Ok(Some(value));
    }
    let value = eval::force_promise(value)?;
    Ok(state.in_slots().then_some(value))
}

/// `UseMethod("generic")` or `UseMethod("generic", object)` from a slot frame, when the body returns its value at once (a generic
/// whose body is the call), dispatched without giving the frame an environment.
unsafe fn slot_use_method(
    state: &Activation<'_, '_>,
    instruction: Instruction,
    generic: Sexp,
    object: Option<Sexp>,
) -> R<Option<Sexp>> {
    // `UseMethod` does not return to the body, so the path is only exact when the body would
    // return the call's value anyway.
    let next = instruction.at + 1 + operand_count(Op::CallGeneric);
    if op_of(*state.code.add(next) as u32) != Some(Op::Return)
        || *state.code.add(next + 1) as usize != instruction.operand(0)
    {
        return Ok(None);
    }
    let formals = state.machine.function.car();
    let first = if formals.kind() == SexpType::Pairlist {
        match slot_named(state, formals.tag()) {
            Some(slot) => slot,
            None => return Ok(None),
        }
    } else {
        std::ptr::null_mut()
    };
    // Through R 4.3 the method sees the generic's locals, which only an environment can show.
    let names = state.constants.element(SLOTS).element(1);
    let locals =
        (length(formals)..length(names)).any(|index| *state.header.add(1 + index) != unbound());
    if locals && !crate::system::r_at_least(4, 4) {
        return Ok(None);
    }
    let owner = (*state.interp).slot_owner as usize;
    dispatch::use_method_in_slots(generic, owner, first, object)
}

/// A builtin call from a slot frame whose arguments the slots answer: `...`, a constant, or a
/// local whose slot holds a value or a promise. `list(...)` and `c(x, ...)` then leave the frame
/// without an environment. `None` when an argument needs one, or when forcing an argument gave
/// the frame its environment, in which case the generic path evaluates the arguments again from
/// it (the promises it meets are forced already, so nothing runs twice).
///
/// `target` is the slot that `target <- call` assigns when the call's first argument reads it
/// (`x <- f(x, ...)`): the builtin may then update that value in place when the slot holds its
/// only reference (see [`slot_reusable`]).
unsafe fn slot_builtin_call(
    state: &Activation<'_, '_>,
    call: Sexp,
    function: Sexp,
    target: Option<*mut Sexp>,
) -> R<Option<Sexp>> {
    let dots = symbols().dots;
    let missing = missing_arg();
    let mut cell = call.cdr();
    while cell.kind() == SexpType::Pairlist {
        let expression = cell.car();
        let fits = match expression.kind() {
            _ if expression == missing => true,
            SexpType::Symbol if expression == dots => slot_named(state, dots).is_some(),
            SexpType::Symbol => {
                eval::dot_dot_index(expression).is_none()
                    && slot_named(state, expression).is_some_and(|slot| {
                        let value = *slot;
                        value != missing && value != unbound()
                    })
            }
            SexpType::Language | SexpType::Promise | SexpType::Bytecode | SexpType::Dots => false,
            _ => true,
        };
        if !fits {
            return Ok(None);
        }
        cell = cell.cdr();
    }
    let definition = eval::builtin_def(function)?;
    let mut args = eval::Args::new();
    let collected = slot_builtin_args(state, call, definition.keep_missing, &mut args);
    // As in `eval::evaluate_args`, each value counts as referenced only while the later
    // arguments evaluate.
    for arg in &args {
        arg.value.decrement_refcount();
    }
    collected?;
    if !state.in_slots() {
        return Ok(None);
    }
    // Dispatch reads the caller's environment, which only an object argument can lead to.
    let env = if definition.generic != Generic::None && args.iter().any(|arg| arg.value.is_object())
    {
        state.env(Reify::Site("builtin call"))?
    } else {
        nil()
    };
    if definition.generic != Generic::None
        && let Some(value) = dispatch::try_builtin_dispatch(definition, call, function, &args, env)?
    {
        return Ok(Some(value));
    }
    match target {
        Some(slot) if definition.reuses_first_argument => {
            let reusable = slot_reusable(state, slot, &args);
            eval::call_builtin_reusing(definition, call, function, &args, env, reusable).map(Some)
        }
        _ => call_builtin(definition, call, function, &args, env).map(Some),
    }
}

/// The first argument of `x <- f(x, ...)` when the update may happen in place: `slot`, the slot
/// of `x`, holds the value itself and is its only reference, no other argument is the same
/// object, and no register of the slot frame's expressions holds it uncounted. A slot frame
/// without an environment is visible to nothing else, so the slot's reference is the binding's;
/// a value bound through a promise stays shared with the promise, which the call's argument
/// list still holds.
unsafe fn slot_reusable(state: &Activation<'_, '_>, slot: *mut Sexp, args: &[Arg]) -> Option<Sexp> {
    let subject = args.first()?.value;
    if *slot != subject || args[1..].iter().any(|arg| arg.value == subject) {
        return None;
    }
    slot_owns(state, subject).then_some(subject)
}

/// Whether a value a slot of the frame holds directly is that slot's alone: its only reference
/// is the slot's, and no register of the frame's expressions holds it uncounted. The body of the
/// innermost frame trusts its compiler as `eval::reuse_permitted` does for a heap frame: a
/// register it left holding the value, such as the `m` read for `m[, t]` in
/// `m[, t + 1] <- m[, t] * k`, is dead. Promise code and a frame below the innermost one may
/// run while registers of other code still hold the value as an argument, so they scan.
unsafe fn slot_owns(state: &Activation<'_, '_>, value: Sexp) -> bool {
    if value.refcount() != 1 {
        return false;
    }
    let registers = &interp().bytecode.registers;
    let header = state.header.offset_from(registers.as_ptr()) as usize;
    if state.machine.function != nil()
        && interp().frames.last().is_some_and(|frame| {
            frame.slots as usize == header && frame.function == state.machine.function
        })
    {
        return true;
    }
    let start = header + 1 + length(state.constants.element(SLOTS).element(1));
    registers
        .get(start..)
        .is_some_and(|registers| !registers.contains(&value))
}

/// `x[i] <- value`, `x[[i]] <- value` or `x$name <- value` on a slot frame's slot, without
/// making its environment, for any `x` that is not an object (an object dispatches, and its
/// method may look at the frame). The compiler guarded the replacement function as the base
/// primitive, so its own code runs here directly, reporting against the whole assignment as the
/// complex assignment does. The slot's value changes in place when the slot alone holds it, as
/// `complex_assign` updates a binding's; a value anything else holds, such as another
/// variable's after `y <- x` or a formal's forced argument, is copied first, which keeps the
/// copy semantics. Returns `false`, changing nothing, when the general assignment must run.
unsafe fn assign_slot_element(
    state: &Activation<'_, '_>,
    slot: *mut Sexp,
    variable: Sexp,
    (index, value, kind): (Sexp, Sexp, ElementKind),
    (whole, setter_call): (Sexp, Sexp),
) -> R<bool> {
    let current = *slot;
    if assign_slot_element_in_place(state, slot, index, value, kind)? {
        return Ok(true);
    }
    // Forcing a formal's promise ran code that may have given the frame its environment.
    if *state.header != nil() {
        return Ok(false);
    }
    let target = if current.kind() == SexpType::Promise {
        current.car()
    } else {
        current
    };
    let replaceable = matches!(
        target.kind(),
        SexpType::Nil
            | SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::String
            | SexpType::Raw
            | SexpType::List
            | SexpType::Expression
    );
    if !replaceable || target.is_object() || (kind == ElementKind::Dollar && length(index) != 1) {
        return Ok(false);
    }
    moves::modifying(target)?;
    let heap = &mut *state.heap;
    let object = if current == target && slot_owns(state, target) {
        target
    } else {
        let was_array = target.is_array();
        let copy = values::shallow_duplicate_attr(target)?;
        copy.set_array(was_array);
        heap.root(copy);
        copy
    };
    let interpreter = interp();
    let outer = interpreter
        .assignment_call
        .replace((setter_call, whole, interpreter.frames.len()));
    let result = match kind {
        ElementKind::Single => crate::subset::subassign(object, &[index], value, setter_call),
        ElementKind::Double => crate::subset::subassign2(object, &[index], value, setter_call),
        ElementKind::Dollar => {
            crate::subset::dollar_assign(object, index.element(0), value, setter_call)
        }
    };
    interp().assignment_call = outer;
    let result = result?;
    let result = values::unwrap_assigned(result, current == result);
    // A warning's calling handler may have looked at the frame, which then holds the variable
    // in its environment instead of the slot.
    if *state.header != nil() {
        let env = *state.header;
        heap.root(result);
        eval::assign_local(variable, result, env, heap.frame_cell(env, variable))?;
    } else {
        heap.store_slot(&mut *slot, result);
    }
    Ok(true)
}

/// [`assign_element`] on a slot's value: the slot's value changes in place when the slot owns
/// it, under the rule `x <- f(x, ...)` follows (see [`slot_reusable`]); a value the slot shares,
/// such as a formal's forced argument, is copied, and the copy (or a vector grown by an
/// appended name) is stored in the slot.
unsafe fn assign_slot_element_in_place(
    state: &Activation<'_, '_>,
    slot: *mut Sexp,
    index: Sexp,
    value: Sexp,
    kind: ElementKind,
) -> R<bool> {
    let current = *slot;
    let target = if current.kind() == SexpType::Promise {
        let target = eval::force_promise(current)?;
        // Forcing ran code that may have given the frame its environment, which then holds
        // the variable instead of the slot.
        if *state.header != nil() {
            return Ok(false);
        }
        target
    } else {
        current
    };
    if target == unbound() || target == missing_arg() || target.kind() == SexpType::Promise {
        return Ok(false);
    }
    let owned = current == target && slot_owns(state, target);
    let Some(result) = assign_element(target, index, value, kind, |_| owned)? else {
        return Ok(false);
    };
    if result != current {
        (*state.heap).store_slot(&mut *slot, result);
    }
    Ok(true)
}

/// The value of a compiled assignment's subscript register in a slot frame, read without
/// making the environment: `None` when it is a formal whose slot holds no value yet.
unsafe fn slot_subscript(state: &Activation<'_, '_>, index: Sexp) -> Option<Sexp> {
    if index.kind() != SexpType::Symbol {
        return Some(index);
    }
    let value = *slot_named(state, index)?;
    let value = if value.kind() == SexpType::Promise {
        value.car()
    } else {
        value
    };
    (value != unbound() && value != missing_arg() && value.kind() != SexpType::Promise)
        .then_some(value)
}

/// The values of [`slot_builtin_call`]'s arguments, each counted as referenced and rooted.
unsafe fn slot_builtin_args(
    state: &Activation<'_, '_>,
    call: Sexp,
    keep_missing: bool,
    args: &mut eval::Args,
) -> R<()> {
    let dots = symbols().dots;
    let missing = missing_arg();
    let heap = crate::heap();
    let mut push = |args: &mut eval::Args, tag: Sexp, item: Sexp| -> R<()> {
        let value = if item == missing {
            if !keep_missing {
                return Err(error_in(
                    call,
                    format!("argument {} is empty", args.len() + 1),
                ));
            }
            missing
        } else {
            eval::value_of(item)?
        };
        heap.root(value);
        value.increment_refcount();
        args.push(Arg { tag, value });
        Ok(())
    };
    let mut cell = call.cdr();
    while cell.kind() == SexpType::Pairlist {
        let expression = cell.car();
        if expression == dots {
            activity::count(Counter::DotsCalls);
            let Some(slot) = slot_named(state, dots) else {
                panic!("a slot frame lost the `...` slot it was checked to have");
            };
            let mut item = *slot;
            if item.kind() != SexpType::Dots && item != missing && item != unbound() {
                return Err(error("'...' used in an incorrect context"));
            }
            while item.kind() == SexpType::Dots {
                push(args, item.tag(), item.car())?;
                if !state.in_slots() {
                    return Ok(());
                }
                item = item.cdr();
            }
        } else if expression.kind() == SexpType::Symbol && expression != missing {
            let Some(slot) = slot_named(state, expression) else {
                panic!("a slot frame lost a slot it was checked to have");
            };
            push(args, cell.tag(), *slot)?;
        } else {
            push(args, cell.tag(), expression)?;
        }
        if !state.in_slots() {
            return Ok(());
        }
        cell = cell.cdr();
    }
    Ok(())
}

/// The arguments of a closure call made from a slot frame that has no environment: a constant
/// as itself, for an `eager` call (see [`eval::eager_args_into`]) a variable as its value, a
/// name or compiled code as a promise over the frame's slots ([`eval::slot_promise`]), and
/// `...` as what it collected. With `references`, for a callee that takes its arguments straight into slots,
/// a name becomes an argument reference ([`eval::argument_reference`]) and compiled code a code
/// reference ([`eval::code_reference`]) instead, which need no allocation and no environment.
/// `false`, with `args` cleared, when an argument needs the frame's environment itself: `..N`, code that was not compiled, or code that uses a local the frame keeps in no
/// slot.
unsafe fn slot_promises_into(
    state: &Activation<'_, '_>,
    arguments: Sexp,
    eager: bool,
    references: bool,
    args: &mut eval::Args,
) -> R<bool> {
    args.clear();
    let owner = (*state.interp).slot_owner as usize;
    let mark = (*state.interp).references_used;
    let unforced = (&(*state.interp).frames)[owner].unforced;
    let missing = missing_arg();
    // One pass builds the arguments and gives up at the first one that needs the frame's
    // environment, which is rare, undoing what it made: references go back to the pool, and
    // the frame's count of unforced promises returns to what it was, since a promise made
    // over a pending environment counts itself and one made over a reified one does not.
    let mut cell = arguments;
    while cell.kind() == SexpType::Pairlist {
        let expression = cell.car();
        let value = match expression.kind() {
            _ if expression == missing => Some(expression),
            SexpType::Symbol if expression == symbols().dots => {
                if slot_dots_into(state, references, args)? {
                    cell = cell.cdr();
                    continue;
                }
                None
            }
            SexpType::Symbol if eval::dot_dot_index(expression).is_some() => None,
            SexpType::Symbol => Some(
                match eager.then(|| slot_eager_argument(state, expression)) {
                    Some(Some(value)) => value,
                    _ if references => {
                        let slot = slot_index(state, expression)
                            .map_or(eval::NO_REFERENCE_SLOT, |index| index as u32);
                        eval::argument_reference(owner, slot, expression)?
                    }
                    _ => eval::slot_promise(owner, expression)?,
                },
            ),
            SexpType::Bytecode if expression.cdr().element(SLOTS) == nil() => None,
            SexpType::Bytecode if references => Some(eval::code_reference(owner, expression)?),
            // Code the compiler rejected (it defines a function, say) runs uncompiled over the
            // pending environment, so the frame gets one only if the callee forces it.
            SexpType::Bytecode | SexpType::Language => Some(eval::slot_promise(owner, expression)?),
            SexpType::Promise => None,
            _ => Some(expression),
        };
        let Some(value) = value else {
            (&mut (*state.interp).frames)[owner].unforced = unforced;
            if (*state.interp).references_used != mark {
                eval::release_references(mark);
            }
            args.clear();
            return Ok(false);
        };
        args.push(Arg {
            tag: cell.tag(),
            value,
        });
        cell = cell.cdr();
    }
    Ok(true)
}

/// Whether a `...` slot holding `value` passes on as it is: the arguments it collected, or
/// none. Anything else is the error a heap frame reports.
unsafe fn forwardable_dots(value: Sexp) -> bool {
    value.kind() == SexpType::Dots || value == missing_arg() || value == unbound()
}

/// Appends the arguments the slot frame's `...` collected, which are its caller's promises and
/// values and pass on unchanged, so forwarding `...` never needs the frame's environment.
/// Without `references`, for a callee that may keep its arguments past the call, an argument
/// reference among them becomes the ordinary promise it stands for first
/// ([`eval::detach_reference`]). `false`, appending nothing, when the frame has no `...` slot
/// or it holds something else.
unsafe fn slot_dots_into(
    state: &Activation<'_, '_>,
    references: bool,
    args: &mut eval::Args,
) -> R<bool> {
    let Some(slot) = slot_named(state, symbols().dots) else {
        return Ok(false);
    };
    let mut item = *slot;
    if !forwardable_dots(item) {
        return Ok(false);
    }
    activity::count(Counter::DotsCalls);
    while item.kind() == SexpType::Dots {
        let value = item.car();
        if !references && eval::is_reference(value) {
            eval::detach_reference(value)?;
        }
        args.push(Arg {
            tag: item.tag(),
            value,
        });
        item = item.cdr();
    }
    Ok(true)
}

/// [`effects::eager_argument`] in a slot frame: the slot's value when `symbol` has a bound
/// slot, and otherwise the binding outside the frame.
unsafe fn slot_eager_argument(state: &Activation<'_, '_>, symbol: Sexp) -> Option<Sexp> {
    if let Some(slot) = slot_named(state, symbol) {
        let value = *slot;
        if value == missing_arg() {
            return None;
        }
        if value != unbound() {
            if value.kind() == SexpType::Promise && value.car() != unbound() {
                return Some(value.car());
            }
            return Some(value);
        }
    }
    effects::eager_argument(symbol, state.machine.function.tag())
}

#[inline(always)]
unsafe fn op_call_builtin_rebinding(
    state: &mut Activation<'_, '_>,
    instruction: Instruction,
) -> R<Flow> {
    let function = instruction.input(1);
    let call = state.constant(instruction, 2);
    let first = instruction.operand(3);
    let count = instruction.operand(4);
    let target = state.constant(instruction, 5);
    let definition = eval::builtin_def(function)?;
    let mut args = eval::Args::new();
    let mut cell = call.cdr();
    for index in 0..count {
        if cell.car() == missing_arg() && !definition.keep_missing {
            return Err(error_in(call, format!("argument {} is empty", index + 1)));
        }
        args.push(Arg {
            tag: cell.tag(),
            value: instruction.register(first + index),
        });
        cell = cell.cdr();
    }
    let reusable = if definition.reuses_first_argument {
        rebinding_reusable(
            state,
            target,
            &args,
            first..first + count,
            instruction.operand(0),
        )?
    } else {
        None
    };
    let value = if reusable.is_some() {
        eval::call_builtin_reusing(definition, call, function, &args, *state.header, reusable)?
    } else {
        call_builtin(definition, call, function, &args, *state.header)?
    };
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

/// The first argument of `target <- f(target, ...)` when `f` may update it in place: `target`'s
/// binding (its slot, or its cell once the frame has an environment) holds the value with the
/// only reference, no other argument is the same object, and no register of the frame's
/// expressions holds it uncounted. `arguments` are the call's own argument registers and `dst`
/// its result register, which a previous run of the same assignment may have left holding it.
unsafe fn rebinding_reusable(
    state: &Activation<'_, '_>,
    target: Sexp,
    args: &[Arg],
    arguments: Range<usize>,
    dst: usize,
) -> R<Option<Sexp>> {
    let Some(subject) = args.first().map(|arg| arg.value) else {
        return Ok(None);
    };
    if subject.refcount() != 1 || args[1..].iter().any(|arg| arg.value == subject) {
        return Ok(None);
    }
    let bound = if state.in_slots() {
        slot_named(state, target).is_some_and(|slot| *slot == subject)
    } else {
        crate::heap()
            .frame_cell(*state.header, target)
            .is_some_and(|cell| {
                !cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED) && cell.cdr() == subject
            })
    };
    if !bound {
        return Ok(None);
    }
    Ok((!expression_registers_hold(state, subject, arguments, dst)).then_some(subject))
}

/// Whether a register of the activation's expressions holds `subject` uncounted, besides its
/// `arguments` registers and its result register `dst`, which a previous run of the same
/// assignment may have left holding it.
unsafe fn expression_registers_hold(
    state: &Activation<'_, '_>,
    subject: Sexp,
    arguments: Range<usize>,
    dst: usize,
) -> bool {
    let registers = &interp().bytecode.registers;
    let start = state.header.offset_from(registers.as_ptr()) as usize
        + 1
        + length(state.constants.element(SLOTS).element(1));
    let base = state.registers.offset_from(registers.as_ptr()) as usize;
    // The scan ends at the activation's own registers: tier 1 caches variable values in cells
    // above them, and it writes those back and forgets them before this opaque step, so a
    // cached copy of the subject is never read again.
    let end = base + values::ints(state.constants.element(REGISTERS))[0] as usize;
    let arguments = base + arguments.start..base + arguments.end;
    registers
        .get(start..end.min(registers.len()))
        .unwrap_or_default()
        .iter()
        .zip(start..)
        .any(|(&value, index)| {
            value == subject && !arguments.contains(&index) && index != base + dst
        })
}

#[inline(always)]
unsafe fn op_call_rebinding(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let function = instruction.input(1);
    let call = state.constant(instruction, 2);
    let target = state.constant(instruction, 3);
    // A slot frame keeps its slots: a builtin may update the slot's value in place, and a
    // closure may take the slot's value as a move ([`moves::call_moving_from`]).
    if state.in_slots()
        && let Some(slot) = slot_named(state, target)
    {
        let value = match function.kind() {
            SexpType::Builtin => slot_builtin_call(state, call, function, Some(slot))?,
            SexpType::Closure => slot_call_moving(state, instruction, function, target, slot)?,
            _ => None,
        };
        if let Some(value) = value {
            state.record(instruction.at, value);
            instruction.output(0, value);
            return Ok(Flow::Next);
        }
        if function.kind() == SexpType::Closure {
            return op_call_generic(state, instruction);
        }
    }
    let value = eval::rebind_call(
        call,
        function,
        target,
        state.env(Reify::Site("rebinding call"))?,
    )?;
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

/// `target <- function(target, ...)` for the closure `function` from a slot frame, whose `slot`
/// binds `target`, moving the slot's value into the closure's parameter as a call from an
/// environment would. `None` when the move does not apply, and the caller makes the ordinary
/// call.
unsafe fn slot_call_moving(
    state: &Activation<'_, '_>,
    instruction: Instruction,
    function: Sexp,
    target: Sexp,
    slot: *mut Sexp,
) -> R<Option<Sexp>> {
    let call = state.constant(instruction, 2);
    let value = *slot;
    // Unlike the environment path, which scans every register, scanning this activation's own
    // expression registers is enough. A value that reaches the slot through a promise is counted
    // by that promise too, so it reads as shared and never moves, and tier 1 keeps its boxed
    // registers in this window, so the scan sees compiled code's copies as well.
    if expression_registers_hold(state, value, 0..0, instruction.operand(0)) {
        return Ok(None);
    }
    let registers = interp().bytecode.registers.as_ptr();
    let caller = moves::Caller {
        binding: moves::Binding::Slot {
            header: state.header.offset_from(registers) as usize,
            position: slot.offset_from(state.header) as usize - 1,
        },
        heads: moves::Heads::Frame((*state.interp).slot_owner as usize),
        sys_parent: nil(),
    };
    moves::call_moving_from(call, function, target, value, caller, |args| {
        let arguments = closure_arguments(state, instruction, call)?;
        let mut others = eval::Args::new();
        if !slot_promises_into(state, arguments.cdr(), false, false, &mut others)? {
            return Ok(false);
        }
        args.extend(others);
        Ok(true)
    })
}

#[inline(always)]
unsafe fn op_call_tail_update(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let function = instruction.input(1);
    let call = state.constant(instruction, 2);
    let target = state.constant(instruction, 3);
    let value = eval::tail_update(
        call,
        function,
        target,
        state.env(Reify::Site("tail update"))?,
    )?;
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_field(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let object = instruction.input(1);
    let name = state.constant(instruction, 2);
    let call = state.constant(instruction, 3);
    let shape = state.constant(instruction, 4);
    let value = match crate::records::cached_field(object, name, shape)? {
        Some(value) => value,
        // Only an object dispatches, to a method that may look at the frame.
        None if object.is_object() => crate::subset::dollar_value(
            object,
            name.car(),
            call,
            state.env(Reify::Site("`$` field"))?,
        )?,
        None => crate::subset::dollar(object, name.car(), call)?,
    };
    record_feedback(state.constants, instruction.at, value);
    (*state.interp).visible = true;
    instruction.output(0, value);
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_item(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let object = instruction.input(1);
    let index = state.constant(instruction, 2);
    let shape = state.constant(instruction, 3);
    match crate::records::cached_item(object, index, shape)? {
        Some(value) => {
            state.record(instruction.at, value);
            (*state.interp).visible = true;
            instruction.output(0, value);
            Ok(Flow::Next)
        }
        None => Ok(Flow::Goto(instruction.operand(4))),
    }
}

#[inline(always)]
unsafe fn op_match_shape(_state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    if !crate::records::has_layout(instruction.input(0), instruction.operand(1) as u32) {
        return Ok(Flow::Goto(instruction.operand(2)));
    }
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_match_tag(_state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    if crate::records::plain_tag(instruction.input(0)) != Some(instruction.operand(1) as u32) {
        return Ok(Flow::Goto(instruction.operand(2)));
    }
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_match_literal(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let pattern = state.constant(instruction, 1);
    if !crate::records::literal_matches(pattern, instruction.input(0)) {
        return Ok(Flow::Goto(instruction.operand(2)));
    }
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_slot(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let value = crate::records::slot_value(instruction.input(1), instruction.operand(2))?;
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_match_fail(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let call = state.constant(instruction, 1);
    Err(crate::records::no_arm_matches(instruction.input(0), call))
}

#[inline(always)]
unsafe fn op_construct(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let slots = std::slice::from_raw_parts(
        instruction.registers.add(instruction.operand(2)),
        instruction.operand(3),
    );
    let value = crate::records::construct(instruction.operand(1) as u32, slots)?;
    (*state.interp).visible = true;
    instruction.output(0, value);
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_move(_state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    instruction.output(0, instruction.input(1));
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_switch_arm(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let table = state.constant(instruction, 2);
    let arm = switch_arm(instruction.input(1), table)?;
    let number = values::ints(table.element(SWITCH_NUMBERS))[arm] as usize;
    instruction.output(0, pool_constant(state, number));
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_bound_eval(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let expression = state.constant(instruction, 1);
    let names = state.constant(instruction, 2);
    let bound = std::slice::from_raw_parts(
        instruction.registers.add(instruction.operand(3)),
        instruction.operand(4),
    );
    let value = crate::records::eval_bound_registers(
        expression,
        names,
        bound,
        state.env(Reify::Site("bound evaluation"))?,
    )?;
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_match_condition(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let guard = state.constant(instruction, 2);
    let call = state.constant(instruction, 3);
    let holds = crate::records::guard_holds(instruction.input(1), guard, call)?;
    instruction.output(0, values::scalar_logical(i32::from(holds))?);
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_branch_unless_same(
    state: &mut Activation<'_, '_>,
    instruction: Instruction,
) -> R<Flow> {
    if instruction.input(0) != state.constant(instruction, 1) {
        return Ok(Flow::Goto(instruction.operand(2)));
    }
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_branch_if_object(
    _state: &mut Activation<'_, '_>,
    instruction: Instruction,
) -> R<Flow> {
    if instruction.input(0).is_object() {
        return Ok(Flow::Goto(instruction.operand(1)));
    }
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_dispatch_subject(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let function = instruction.input(1);
    let call = state.constant(instruction, 2);
    let definition = eval::builtin_def(function)?;
    let subject = instruction.input(3);
    let value = match slot_subject_call(state, definition, call, function, subject)? {
        Some(value) => value,
        None => {
            let mut args = eval::Args::new();
            match dispatch::dispatch_evaluated_subject(
                definition,
                call,
                function,
                call.cdr(),
                subject,
                state.env(Reify::Site("dispatch subject"))?,
                &mut args,
            )? {
                Some(value) => value,
                None => eval::call_builtin(definition, call, function, &args, *state.header)?,
            }
        }
    };
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

/// A dispatch-or-eval special called from a slot frame once its subject evaluated to
/// `subject`, when the slots answer every other argument: an empty argument, a constant, or a
/// local holding a value or a promise. The order is GNU R's `DispatchOrEval`: an object subject
/// looks for its method first, and only without one do the other arguments evaluate, a missing
/// one passing on as an empty argument (`i` in `g <- function(i) f(x, i); g()`). The method
/// lookup starts at the enclosure, which answers as the frame would because no local is named
/// like a method of the generic, so the frame gets an environment only for a method to run in.
///
/// Dispatch is decided once the lookup finds no method, so the primitive runs without the second
/// lookup [`call_builtin`] would make, as GNU R's subset primitives call their default code.
///
/// `None` sends the call to the environment's path: an argument the slots do not answer, an S4
/// or frame subject, a local that could bind a method or make the frame a top environment, and
/// forcing an argument that gave the frame its environment (the promises it forced stay forced,
/// so nothing runs twice).
unsafe fn slot_subject_call(
    state: &Activation<'_, '_>,
    definition: &'static BuiltinDef,
    call: Sexp,
    function: Sexp,
    subject: Sexp,
) -> R<Option<Sexp>> {
    if !state.in_slots() {
        return Ok(None);
    }
    let missing = missing_arg();
    let arguments = call.cdr();
    let mut cell = arguments.cdr();
    while cell.kind() == SexpType::Pairlist {
        let expression = cell.car();
        let fits = match expression.kind() {
            _ if expression == missing => true,
            SexpType::Symbol => {
                expression != symbols().dots
                    && eval::dot_dot_index(expression).is_none()
                    && slot_named(state, expression).is_some_and(|slot| *slot != unbound())
            }
            SexpType::Language | SexpType::Promise | SexpType::Bytecode | SexpType::Dots => false,
            _ => true,
        };
        if !fits {
            return Ok(None);
        }
        cell = cell.cdr();
    }
    if subject.is_object() {
        if subject.has_gp(gp::S4_OBJECT)
            || (definition.name == "[" && (interp().extensions.tables.is_frame)(subject))
        {
            return Ok(None);
        }
        let generic = definition.name.as_bytes();
        let names = state.constants.element(SLOTS).element(1);
        let shadows = names.slice::<Sexp>().iter().any(|&name| {
            let name = name.car().bytes();
            name == b".packageName"
                || (name.len() > generic.len()
                    && name.starts_with(generic)
                    && name[generic.len()] == b'.')
        });
        if shadows {
            return Ok(None);
        }
        if let Some(method) =
            dispatch::subject_method(definition, subject, state.machine.enclosure())?
        {
            let env = state.env(Reify::Site("dispatch subject: method"))?;
            return dispatch::apply_subject_method(call, method, subject, env).map(Some);
        }
    }
    let heap = crate::heap();
    let mut args = eval::Args::new();
    heap.root(subject);
    subject.increment_refcount();
    args.push(Arg {
        tag: arguments.tag(),
        value: subject,
    });
    let mut collect = |args: &mut eval::Args| -> R<bool> {
        let mut cell = arguments.cdr();
        while cell.kind() == SexpType::Pairlist {
            let expression = cell.car();
            let value = if expression == missing {
                missing
            } else if expression.kind() == SexpType::Symbol {
                let Some(slot) = slot_named(state, expression) else {
                    panic!("a slot frame lost a slot it was checked to have");
                };
                let content = *slot;
                if eval::value_is_missing(content)? {
                    missing
                } else {
                    eval::value_of(content)?
                }
            } else {
                expression
            };
            heap.root(value);
            value.increment_refcount();
            args.push(Arg {
                tag: cell.tag(),
                value,
            });
            if !state.in_slots() {
                return Ok(false);
            }
            cell = cell.cdr();
        }
        Ok(true)
    };
    let collected = collect(&mut args);
    for arg in &args {
        arg.value.decrement_refcount();
    }
    if !collected? {
        return Ok(None);
    }
    eval::call_builtin(definition, call, function, &args, nil()).map(Some)
}

#[inline(always)]
unsafe fn op_ast(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let expression = state.constant(instruction, 1);
    let value = eval::eval(expression, state.env(Reify::Site("uncompiled expression"))?)?;
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_super_assign(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let symbol = state.constant(instruction, 0);
    let value = instruction.input(1);
    eval::set_var(symbol, value, state.machine.enclosure())?;
    (*state.interp).visible = false;
    Ok(Flow::Next)
}

/// A base closure's `.Internal` inlined ([`Op::Internal`]): the internal runs on the evaluated
/// arguments, as in GNU R's compiled code.
#[inline(always)]
unsafe fn op_internal(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let first = instruction.operand(3);
    let call = state.constant(instruction, 1);
    let function = state.constant(instruction, 2);
    let definition = eval::builtin_def(function)?;
    let mut args = eval::Args::new();
    for index in 0..instruction.operand(4) {
        args.push(Arg {
            tag: nil(),
            value: instruction.register(first + index),
        });
    }
    // The closure's frame would enclose base's namespace, which is where an internal that
    // dispatches (`paste` and `match` on objects) looks up the generic, not the caller's frame.
    let namespace = crate::heap().known().base_namespace;
    let value = eval::call_inlined_internal(definition, call, function, &args, namespace)?;
    state.record(instruction.at, value);
    instruction.output(0, value);
    (*state.interp).visible = true;
    Ok(Flow::Next)
}

unsafe fn op_missing(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let symbol = state.constant(instruction, 1);
    let value = values::scalar_bool(slot_missing(state, symbol)?)?;
    state.record(instruction.at, value);
    (*state.interp).visible = true;
    instruction.output(0, value);
    Ok(Flow::Next)
}

/// `package::symbol` through the cached namespace binding, and through the evaluator's lookup,
/// which may load the namespace, when the cache is empty or stale.
unsafe fn op_namespace(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let package = state.constant(instruction, 1);
    let symbol = state.constant(instruction, 2);
    let slot = instruction.operand(4);
    let cache = state.cache;
    let cached = if *cache.epochs.add(slot) == namespace_stamp(package, symbol) {
        bound_value(*cache.slots.add(slot * 2 + 1))
    } else {
        None
    };
    let value = match cached {
        Some(value) => value,
        None => {
            let exported = instruction.operand(3) != 0;
            let (value, cell) = namespace_binding(package, symbol, exported)?;
            if let Some(cell) = cell {
                let heap = crate::heap();
                heap.set_element(cache.caches, slot * 2, package);
                heap.set_element(cache.caches, slot * 2 + 1, cell);
                *cache.epochs.add(slot) = namespace_stamp(package, symbol);
            }
            value
        }
    };
    state.record(instruction.at, value);
    (*state.interp).visible = true;
    instruction.output(0, value);
    Ok(Flow::Next)
}

/// What a cached `package::symbol` depends on besides the cell's value: the registry's binding
/// of the package, the namespace's `.__NAMESPACE__.` and its `exports` table, and every binding
/// of the name on the way from the namespace to the cell. [`namespace_binding`] watches
/// the bindings it read, so changing any of them moves one of these stamps, and stamps only
/// grow, so their sum moves too.
#[inline(always)]
unsafe fn namespace_stamp(package: Sexp, symbol: Sexp) -> u64 {
    let heap = crate::heap();
    let symbols = symbols();
    heap.lookup_stamp(package)
        .wrapping_add(heap.lookup_stamp(symbol))
        .wrapping_add(heap.lookup_stamp(symbols.dot_namespace))
        .wrapping_add(heap.lookup_stamp(symbols.exports))
}

/// The value a binding cell holds when reading it needs no evaluation: not an active binding,
/// an unforced promise, or a missing or unbound value.
#[inline(always)]
unsafe fn bound_value(cell: Sexp) -> Option<Sexp> {
    if cell == nil() || cell.has_gp(gp::ACTIVE_BINDING) {
        return None;
    }
    let mut value = cell.cdr();
    if value.kind() == SexpType::Promise {
        value = value.car();
    }
    (value != unbound() && value != missing_arg()).then_some(value)
}

/// The symbol a side of a literal `::` names: a symbol as written, or a one-element string.
unsafe fn colon_literal(value: Sexp) -> R<Option<Sexp>> {
    match value.kind() {
        SexpType::Symbol if value != missing_arg() && value != symbols().dots => Ok(Some(value)),
        SexpType::String if length(value) == 1 && value.attrib() == nil() => {
            values::install_charsxp(value.element(0)).map(Some)
        }
        _ => Ok(None),
    }
}

/// `missing(symbol)`, read from a slot frame's slots and missing bits when `symbol` is one of
/// its formals, which needs no environment.
unsafe fn slot_missing(state: &Activation<'_, '_>, symbol: Sexp) -> R<bool> {
    if state.in_slots() && symbol != missing_arg() {
        let names = state.constants.element(SLOTS).element(1);
        let formals = length(state.machine.function.car());
        if let Some(position) =
            (0..formals.min(length(names))).find(|&index| names.element(index) == symbol)
        {
            let owner = (*state.interp).slot_owner as usize;
            #[allow(clippy::borrow_deref_ref, clippy::needless_borrow)]
            let missing = (&(*state.interp).frames)
                .get(owner)
                .map_or(0, |frame| frame.missing);
            if missing & (1 << position) != 0 {
                return Ok(true);
            }
            return eval::value_is_missing(*state.header.add(1 + position));
        }
    }
    eval::is_missing(symbol, state.env(Reify::Site("missing()"))?)
}

#[inline(always)]
unsafe fn op_guard(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let symbol = state.constant(instruction, 0);
    let expected = state.constant(instruction, 1);
    let slot = instruction.operand(2);
    // The usual guard is a call to a base function from a closure whose frame cannot bind the
    // name: the cell the cache resolved still holding the expected function is the whole check.
    // A cached cell never becomes active (`makeActiveBinding` refuses a regular binding), and a
    // binding that appears in front of it moves the stamp.
    let enclosure = state.machine.enclosure();
    if instruction.operand(3) == 0
        && !enclosure.has_gp(gp::FUNCTION_FRAME)
        && let Some(cell) = cached_cell(symbol, enclosure, state.cache, slot)
        && cell.cdr() == expected
    {
        return Ok(Flow::Next);
    }
    let found = if instruction.operand(3) == 0 {
        lookup_enclosing_function(symbol, enclosure, state.cache, slot)
    } else {
        lookup_function(
            symbol,
            state.env(Reify::Site("function guard"))?,
            state.cache,
            slot,
            unbound(),
        )
    };
    if !matches!(found, Ok(function) if function == expected) {
        return Ok(Flow::Goto(instruction.operand(4)));
    }
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_resume(_state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    Ok(Flow::Goto(instruction.operand(0)))
}

#[inline(always)]
unsafe fn op_jump_unless(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let call = state.constant(instruction, 1);
    if !as_condition(instruction.input(0), call)? {
        state.record_kind(instruction.at, feedback::BRANCH_TAKEN);
        return Ok(Flow::Goto(instruction.operand(2)));
    }
    state.record_kind(instruction.at, feedback::BRANCH_NOT_TAKEN);
    Ok(Flow::Next)
}

/// The operand of an [`Op::Arithmetic`] whose storage may hold the result: a temporary the
/// `reuse` bits mark that no counted reference holds. Of two candidates, the one of the wider
/// type is preferred, since the wider operand's type is the result's.
#[inline(always)]
unsafe fn arithmetic_destination(left: Sexp, right: Sexp, reuse: usize) -> Option<Sexp> {
    let candidate = |value: Sexp, bit: usize| {
        (reuse & bit != 0 && left != right && value.refcount() == 0).then_some(value)
    };
    let width = |value: Sexp| match value.kind() {
        SexpType::Complex => 2,
        SexpType::Real => 1,
        _ => 0,
    };
    match (candidate(left, 1), candidate(right, 2)) {
        (Some(left), Some(right)) if width(left) < width(right) => Some(right),
        (Some(left), _) => Some(left),
        (None, right) => right,
    }
}

#[inline(always)]
unsafe fn op_arithmetic(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let left = instruction.input(2);
    let right = instruction.input(3);
    let operator = ARITHS[instruction.operand(0)].0;
    let value = match fast_arithmetic(operator, left, right)? {
        Some(value) => {
            quicken_arithmetic(instruction.code, instruction.at, operator, left, right);
            value
        }
        None if let Some(value) = kernels::binary(
            state.constant(instruction, 4),
            operator,
            left,
            right,
            arithmetic_destination(left, right, instruction.operand(6)),
        )? =>
        {
            value
        }
        None => {
            let call = state.constant(instruction, 4);
            let function = state.constant(instruction, 5);
            let definition = eval::builtin_def(function)?;
            let args = [
                Arg {
                    tag: nil(),
                    value: left,
                },
                Arg {
                    tag: nil(),
                    value: right,
                },
            ];
            call_builtin(definition, call, function, &args, *state.header)?
        }
    };
    state.record(instruction.at, value);
    instruction.output(1, value);
    (*state.interp).visible = true;
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_fused(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let program = state.constant(instruction, 1);
    if program == nil() {
        return Ok(Flow::Next);
    }
    let words = values::ints_mut(program);
    match fused_pass(state, instruction, words)? {
        Some(value) => {
            words[fusion::DECLINES] = 0;
            instruction.output(0, value);
            (*state.interp).visible = true;
            Ok(Flow::Goto(instruction.operand(2)))
        }
        None => {
            words[fusion::DECLINES] += 1;
            if words[fusion::DECLINES] >= fusion::GIVE_UP {
                // Every closure sharing this body runs the same code, so they all stop trying,
                // and the program is garbage from here on.
                values::ints_mut(state.machine.code.car())[instruction.at] =
                    op_word(Op::Unfused) as i32;
                crate::heap().set_element(state.constants, instruction.operand(1), nil());
            }
            Ok(Flow::Next)
        }
    }
}

#[inline(always)]
unsafe fn op_unfused(_state: &mut Activation<'_, '_>, _instruction: Instruction) -> R<Flow> {
    Ok(Flow::Next)
}

/// Runs a fused program (laid out as [`fusion`] describes), or declines with `None` when the
/// ordinary code must run instead: an operator name no longer finds its base function, a
/// variable is missing or bound actively, or `kernels::fused` declines the values.
unsafe fn fused_pass(
    state: &mut Activation<'_, '_>,
    instruction: Instruction,
    words: &[i32],
) -> R<Option<Sexp>> {
    let heap = crate::heap();
    // A slot frame without an environment keeps its locals in slots, which the pass reads by
    // name; free names start from the closure's environment either way.
    let env = *state.header;
    let enclosure = state.machine.enclosure();
    let local_slot = |symbol: Sexp| {
        state
            .machine
            .slot_of(symbol)
            .map(|slot| *slot)
            .filter(|&value| value != unbound())
    };
    let cache = state.cache;
    let node_count = words[fusion::NODES] as usize;
    let leaf_count = words[fusion::LEAVES] as usize;
    let guard_count = words[fusion::GUARDS] as usize;
    let guards_end = fusion::HEADER + guard_count * fusion::GUARD;
    let leaves_end = guards_end + leaf_count * fusion::LEAF;
    let leaf_words = &words[guards_end..leaves_end];
    let node_words = &words[leaves_end..leaves_end + node_count * fusion::NODE];
    // The leaves are read in the order the ordinary code reads them. Forcing an argument's
    // promise runs code, which must find the values read before it referenced, as the ordinary
    // code's registers are, so that an assignment it makes copies instead of changing them.
    let mut leaves = [nil(); MAX_NODES];
    for (index, leaf) in leaf_words.chunks_exact(fusion::LEAF).enumerate() {
        if leaf[0] == 2 {
            leaves[index] = instruction.register(leaf[1] as usize);
            continue;
        }
        let constant = state.constants.element(leaf[1] as usize);
        if leaf[0] == 0 {
            leaves[index] = constant;
            continue;
        }
        let slot = leaf[2] as usize;
        let hint = &mut *cache.hints.add(slot);
        let local_value = if env == nil() {
            local_slot(constant)
        } else {
            None
        };
        let (value, local) = match local_value {
            Some(value) => (value, true),
            None => {
                let local_cell = if env == nil() {
                    None
                } else {
                    heap.frame_cell_hinted(env, constant, hint)
                };
                let (cell, local) = match local_cell {
                    Some(cell) => (cell, true),
                    None => match outside_function_frames(constant, enclosure, hint) {
                        Err(cell) => (cell, false),
                        Ok(enclosure) => match cached_cell(constant, enclosure, cache, slot) {
                            Some(cell) => (cell, false),
                            None => return Ok(None),
                        },
                    },
                };
                if cell.has_gp(gp::ACTIVE_BINDING) {
                    return Ok(None);
                }
                (cell.cdr(), local)
            }
        };
        if value == missing_arg() || value == unbound() {
            return Ok(None);
        }
        leaves[index] = if value.kind() != SexpType::Promise {
            value
        } else if value.car() != unbound() {
            value.car()
        } else if local {
            let read = &leaves[..index];
            for &earlier in read {
                heap.root(earlier);
                earlier.increment_refcount();
            }
            let base = state.machine.base;
            let held = base..base + instruction.operand(0);
            let forced = holding(held, || eval::force_promise(value));
            for &earlier in read {
                earlier.decrement_refcount();
            }
            let forced = forced?;
            heap.root(forced);
            forced
        } else {
            return Ok(None);
        };
    }
    // On scalars the ordinary instructions are cheaper than the pass and fusion saves no
    // intermediate vector, so the pass declines and, after enough declines in a row, stops trying.
    if leaves[..leaf_count].iter().all(|&leaf| leaf.length() <= 1) {
        return Ok(None);
    }
    for guard in words[fusion::HEADER..guards_end].chunks_exact(fusion::GUARD) {
        let symbol = state.constants.element(guard[0] as usize);
        let expected = state.constants.element(guard[1] as usize);
        let slot = guard[2] as usize;
        let hint = &mut *cache.hints.add(slot);
        let shadowed = if env == nil() {
            local_slot(symbol).is_some()
        } else {
            heap.frame_cell_hinted(env, symbol, hint).is_some()
        };
        if guard[3] != 0 && shadowed {
            return Ok(None);
        }
        let Ok(enclosure) = outside_function_frames(symbol, enclosure, hint) else {
            return Ok(None);
        };
        let Some(cell) = cached_cell(symbol, enclosure, cache, slot) else {
            return Ok(None);
        };
        if cell.has_gp(gp::ACTIVE_BINDING) || forced_value(cell.cdr()) != expected {
            return Ok(None);
        }
    }
    let mut nodes: SmallVec<[Node; MAX_NODES]> = SmallVec::new();
    for node in node_words.chunks_exact(fusion::NODE) {
        let [a, b, c] = [node[1], node[2], node[3]].map(|word| word as usize);
        nodes.push(match node[0] {
            0 => Node::Leaf(a),
            1 => Node::Binary(ARITHS[a].0, b, c),
            2 => Node::Logic {
                is_and: a != 0,
                left: b,
                right: c,
            },
            3 => Node::Negate(a),
            4 => Node::Not(a),
            _ => Node::Math(MATHS[a].0, b),
        });
    }
    let reduction = match words[fusion::REDUCTION] {
        0 => Some(Reduction::Sum),
        1 => Some(Reduction::Any),
        2 => Some(Reduction::All),
        _ => None,
    };
    // The assignment's `SetVar` names the symbol and its lookup cache; a code image is only
    // checked to make the jump target an instruction, so the pass checks that it is this one.
    let assignment = instruction.code.add(instruction.operand(2));
    let into = if words[fusion::REDUCTION] == fusion::ASSIGNED
        && *assignment == op_word(Op::SetVar) as i32
        && *assignment.add(2) as usize == instruction.operand(0)
    {
        reusable_target(
            state,
            state.constants.element(*assignment.add(1) as usize),
            *assignment.add(3) as usize,
        )
    } else {
        None
    };
    let Some(fused) = kernels::fused(&nodes, &leaves[..leaf_count], reduction, into)? else {
        return Ok(None);
    };
    if !state.feedback.is_null() {
        for (index, leaf) in leaf_words.chunks_exact(fusion::LEAF).enumerate() {
            if leaf[0] == 1 {
                state.record(leaf[3] as usize, leaves[index]);
            }
        }
        for (index, node) in node_words.chunks_exact(fusion::NODE).enumerate() {
            if node[0] == 0 {
                continue;
            }
            let kind = match (fused.elements[index], fused.scalar[index]) {
                (Element::Logical, true) => feedback::LOGICAL_SCALAR,
                (Element::Integer, true) => feedback::INTEGER_SCALAR,
                (Element::Double, true) => feedback::DOUBLE_SCALAR,
                (Element::Logical, false) => feedback::LOGICAL_VECTOR,
                (Element::Integer, false) => feedback::INTEGER_VECTOR,
                (Element::Double, false) => feedback::DOUBLE_VECTOR,
            };
            *state.feedback.add(node[4] as usize) |= kind as i32;
        }
        if reduction.is_some() {
            state.record(words[fusion::REDUCTION_SITE] as usize, fused.value);
        }
    }
    Ok(Some(fused.value))
}

/// The value `symbol` holds in the activation's own frame, when the assignment the fused
/// pass computes may overwrite it: the binding is a plain one holding the only reference, and
/// no register of the frame's expressions holds it uncounted (`slot_owns`,
/// `eval::reuse_permitted`). The compiler keeps such registers dead in the innermost compiled
/// frame, as `may_modify_in_place` counts `x <- call` among the in-place updates.
unsafe fn reusable_target(state: &Activation<'_, '_>, symbol: Sexp, cache: usize) -> Option<Sexp> {
    if state.in_slots() {
        let value = *state.slot(cache)?;
        return slot_owns(state, value).then_some(value);
    }
    let env = *state.header;
    let hint = &mut *state.cache.hints.add(cache);
    let cell = crate::heap().frame_cell_hinted(env, symbol, hint)?;
    let value = cell.cdr();
    (!cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED)
        && value.refcount() == 1
        && eval::reuse_permitted(env, value))
    .then_some(value)
}

#[inline(always)]
unsafe fn op_return(_state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    Ok(Flow::Done(instruction.input(0)))
}

#[inline(always)]
unsafe fn op_for_init(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let call = state.constant(instruction, 2);
    let mut sequence = instruction.input(1);
    if values::inherits(sequence, "factor") {
        sequence = crate::coerce::factor_as_character(sequence)?;
    }
    // A compact sequence is stepped through by its first element and increment; expanding
    // `1:n` would allocate all n elements the loop reads one at a time.
    let steps = values::compact_sequence(sequence).map(|(_, first, step)| (first, step));
    let (first, step) = steps.unwrap_or((0.0, 0.0));
    let sequence = match steps {
        Some(_) => sequence,
        None => values::materialize(sequence)?,
    };
    let count = match sequence.kind() {
        SexpType::Nil => 0,
        SexpType::Pairlist | SexpType::Language => values::pairlist_length(sequence),
        kind if kind.is_vector() => values::length(sequence),
        _ => return Err(error_in(call, "invalid for() loop sequence")),
    };
    // The body must see the sequence as shared, so that modifying the variable it came from
    // copies it instead of changing the elements still to come.
    sequence.increment_refcount();
    match steps {
        // Feedback sees through the compact form, because the compiled loop steps through it
        // the same way.
        Some(_) => state.record_kind(
            instruction.at,
            match sequence.kind() {
                SexpType::Integer => feedback::INTEGER_VECTOR,
                _ => feedback::DOUBLE_VECTOR,
            },
        ),
        None => state.record(instruction.at, sequence),
    }
    instruction.output(1, sequence);
    // Promise code runs without loop states, so only code read from a file could name one here.
    *state
        .machine
        .loops
        .get_mut(instruction.operand(0))
        .ok_or_else(|| error("compiled code names a loop it does not have"))? = LoopState {
        index: 0,
        count,
        first,
        step,
        cell: Some(sequence),
    };
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_for_step(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let variable = state.constant(instruction, 2);
    // A slot frame keeps its loop variable in a slot, so stepping the loop needs no environment.
    let slot = if state.in_slots() {
        slot_at(state, instruction.operand(2))
    } else {
        None
    };
    let env = match slot {
        Some(_) => nil(),
        None => state.env(Reify::Site("for loop"))?,
    };
    let loop_state = state
        .machine
        .loops
        .get_mut(instruction.operand(0))
        .ok_or_else(|| error("compiled code names a loop it does not have"))?;
    if loop_state.index >= loop_state.count {
        // R binds the variable to NULL before the first iteration, which only shows when there
        // is none; binding it on every entry would cost a loop the scalar it overwrites in place.
        if loop_state.count == 0 {
            match slot {
                Some(slot) => {
                    crate::heap().store_slot(&mut *slot, nil());
                    clear_missing(state, slot);
                }
                None => eval::define(variable, nil(), env)?,
            }
        }
        return Ok(Flow::Goto(instruction.operand(3)));
    }
    let sequence = instruction.input(1);
    // The compiler steps the register its `ForInit` filled, which keeps the index in bounds;
    // code read from a file is only verified operand by operand.
    let (first, step) = (loop_state.first, loop_state.step);
    if step == 0.0 && sequence.kind().is_vector() && loop_state.index >= sequence.length() as usize
    {
        return Err(error("compiled code steps a loop over another sequence"));
    }
    let index = loop_state.index;
    loop_state.index += 1;
    let current = match slot {
        Some(slot) => Some(*slot),
        None => crate::heap()
            .frame_cell(env, variable)
            .filter(|cell| !cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED))
            .map(|cell| cell.cdr()),
    };
    let number = first + step * index as f64;
    let overwritten = current.is_some_and(|value| match step == 0.0 {
        true => overwrite_loop_variable(sequence, index, value),
        false => overwrite_scalar(value, sequence.kind(), number),
    });
    if overwritten {
        return Ok(Flow::Next);
    }
    let element = match sequence.kind() {
        SexpType::Integer if step != 0.0 => values::scalar_integer(number as i32)?,
        SexpType::Real if step != 0.0 => values::scalar_real(number)?,
        SexpType::List | SexpType::Expression => sequence.element(index),
        SexpType::Pairlist | SexpType::Language => {
            let cell = loop_state.cell.unwrap_or(sequence);
            loop_state.cell = Some(cell.cdr());
            cell.car()
        }
        _ => crate::subset::vector_element(sequence, index)?,
    };
    match slot {
        Some(slot) => {
            crate::heap().store_slot(&mut *slot, element);
            clear_missing(state, slot);
        }
        None => eval::define(variable, element, env)?,
    }
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_for_end(_state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    instruction.input(0).decrement_refcount();
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_invisible(state: &mut Activation<'_, '_>, _instruction: Instruction) -> R<Flow> {
    (*state.interp).visible = false;
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_visible(state: &mut Activation<'_, '_>, _instruction: Instruction) -> R<Flow> {
    (*state.interp).visible = true;
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_set_srcref(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    (*state.interp).srcref = state.constant(instruction, 0);
    if instruction.operand(1) == 1 && (*state.interp).debugger.is_some() {
        crate::debugger::statement()?;
    }
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_end_statement(_state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    instruction.output(0, nil());
    for register in instruction.operand(1)..instruction.operand(2) {
        instruction.set(register, nil());
    }
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_logic_left(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let call = state.constant(instruction, 2);
    let is_and = instruction.operand(3) == 1;
    let left = eval::logical_operand_of(
        instruction.input(1),
        call,
        if is_and { "&&" } else { "||" },
        "x",
    )?;
    if (is_and && left == 0) || (!is_and && left == 1) {
        instruction.output(0, values::scalar_logical(left)?);
        (*state.interp).visible = true;
        return Ok(Flow::Goto(instruction.operand(4)));
    }
    instruction.output(1, values::scalar_logical(left)?);
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_logic_right(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let call = state.constant(instruction, 3);
    let is_and = instruction.operand(4) == 1;
    let left = instruction.input(1);
    // `LogicLeft` stored a logical scalar there, unless the code was read from a file and lies.
    if left.kind() != SexpType::Logical || left.length() != 1 {
        return Err(error("compiled code holds a malformed `&&` or `||`"));
    }
    let left = values::ints(left)[0];
    let right = eval::logical_operand_of(
        instruction.input(2),
        call,
        if is_and { "&&" } else { "||" },
        "y",
    )?;
    instruction.output(
        0,
        values::scalar_logical(logic_result(is_and, left, right))?,
    );
    (*state.interp).visible = true;
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_closure(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let formals = state.constant(instruction, 1);
    let body = state.constant(instruction, 2);
    let srcref = state.constant(instruction, 3);
    let closure = if state.in_slots() {
        eval::slot_closure(state.machine.header, formals, body, srcref)?
    } else {
        eval::make_closure(
            formals,
            body,
            srcref,
            state.env(Reify::Site("function definition"))?,
        )?
    };
    // Every closure this site creates shares one compiled body, as GNU R compiles inner
    // functions with their outer one. Compiling per closure object would leave a function
    // created anew on each call (a `vapply` callback, say) on the AST evaluator, or recompile it
    // every other call. The code depends on the formals and body only: its inline caches and the
    // optimizing tier both check the enclosure they were filled for.
    if body.kind() != SexpType::Bytecode && worth_compiling(body) {
        let code = compile(body, closure, false)?;
        (*state.heap).set_element(state.constants, instruction.operand(2), code);
        (*state.heap).set_cdr(closure, code);
    }
    instruction.output(0, closure);
    (*state.interp).visible = true;
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_sub_assign(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let call = state.constant(instruction, 4);
    let value = instruction.input(3);
    let variable = state.constant(instruction, 1);
    let kind = match instruction.operand(5) {
        0 => ElementKind::Single,
        1 => ElementKind::Double,
        _ => ElementKind::Dollar,
    };
    let index = instruction.input(2);
    let target = call.cdr().car();
    // A symbol in the register is a formal left for lookup only when the subscript is that
    // formal; any other symbol is a computed subscript, which `[<-` rejects.
    if index.kind() == SexpType::Symbol && index != target.cdr().cdr().car() {
        eval::complex_assign(
            call,
            target,
            value,
            state.env(Reify::Site("subassignment"))?,
            false,
            &evaluated_arguments(target, &[index])?,
        )?;
        instruction.output(0, value);
        (*state.interp).visible = false;
        return Ok(Flow::Next);
    }
    if state.in_slots()
        && let Some(slot) = slot_at(state, instruction.operand(1))
        && let Some(index) = slot_subscript(state, instruction.input(2))
        && assign_slot_element(
            state,
            slot,
            variable,
            (index, value, kind),
            (call, state.constant(instruction, 6)),
        )?
    {
        instruction.output(0, value);
        (*state.interp).visible = false;
        return Ok(Flow::Next);
    }
    let env = state.env(Reify::Site("subassignment"))?;
    let done = match subscript_value(env, index)? {
        Some(index) => {
            assign_element_in_place(env, variable, index, value, kind)?
                || (kind == ElementKind::Single
                    && element_access::assign_subset_in_place(env, variable, index, value)?)
        }
        None => false,
    };
    if !done {
        let evaluated = evaluated_arguments(target, &[index])?;
        eval::complex_assign(call, target, value, env, false, &evaluated)?;
    }
    instruction.output(0, value);
    (*state.interp).visible = false;
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_sub_assign_matrix(
    state: &mut Activation<'_, '_>,
    instruction: Instruction,
) -> R<Flow> {
    let env = state.env(Reify::Site("matrix subassignment"))?;
    let call = state.constant(instruction, 5);
    let value = instruction.input(4);
    let variable = state.constant(instruction, 1);
    let target = call.cdr().car();
    let subscripts = [instruction.input(2), instruction.input(3)];
    let codes = [target.cdr().cdr().car(), target.cdr().cdr().cdr().car()];
    // As in `op_sub_assign`, a symbol other than the subscript's own is a computed one.
    let lookups = subscripts
        .iter()
        .zip(codes)
        .all(|(subscript, code)| subscript.kind() != SexpType::Symbol || *subscript == code);
    let done = lookups
        && match (
            subscript_value(env, subscripts[0])?,
            subscript_value(env, subscripts[1])?,
        ) {
            (Some(row), Some(column)) => {
                assign_matrix_element_in_place(env, variable, row, column, value)?
            }
            _ => false,
        };
    if !done {
        let evaluated = evaluated_arguments(target, &subscripts)?;
        eval::complex_assign(call, target, value, env, false, &evaluated)?;
    }
    instruction.output(0, value);
    (*state.interp).visible = false;
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_replace(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let whole = state.constant(instruction, 7);
    let (first, count) = (instruction.operand(5), instruction.operand(6));
    let value = instruction.register(first + count - 1);
    let replace = |state: &Activation<'_, '_>, env: Sexp| {
        replace_variable(
            state,
            env,
            state.constant(instruction, 1),
            (state.constant(instruction, 2), instruction.operand(3)),
            state.constant(instruction, 4),
            whole,
            std::slice::from_raw_parts_mut(instruction.registers.add(first), count),
        )
    };
    if !(state.in_slots() && replace(state, nil())?) {
        let env = state.env(Reify::Site("replacement call"))?;
        if !replace(state, env)? {
            let target = whole.cdr().car();
            let subscripts =
                std::slice::from_raw_parts(instruction.registers.add(first + 1), count - 2);
            let evaluated = evaluated_arguments(target, subscripts)?;
            eval::complex_assign(whole, target, value, env, false, &evaluated)?;
        }
    }
    instruction.output(0, value);
    (*state.interp).visible = false;
    Ok(Flow::Next)
}

/// Where a backward jump leaves interpretation once the optimizing tier has counted it.
enum BackEdge {
    Continue(usize),
    Finished(R<Sexp>),
    Unwound { pc: usize, unwind: Unwind },
}

/// Counts a backward jump and answers whether it reaches a count [`back_edge`] acts on. Every
/// backward jump of every loop pays this, so it is one increment and two comparisons, inlined
/// into the jump; the rest waits in [`back_edge`] for the counts that need it.
#[inline(always)]
unsafe fn back_edge_due(state: &mut Activation<'_, '_>, tier: &Tier) -> bool {
    let profile = values::ints_mut(state.constants.element(PROFILE));
    let count = profile[profile::BACK_EDGES].saturating_add(1);
    profile[profile::BACK_EDGES] = count;
    count >= tier.back_edge_threshold || count == tier.back_edge_threshold / 2
}

#[inline(never)]
#[cold]
unsafe fn back_edge(state: &mut Activation<'_, '_>, tier: &Tier, pc: usize) -> BackEdge {
    let machine = &mut *state.machine;
    let profile = values::ints_mut(machine.code.cdr().element(PROFILE));
    if profile[profile::TIER_STATE] == profile::TIER_OFF {
        profile[profile::BACK_EDGES] = 0;
        return BackEdge::Continue(pc);
    }
    if profile[profile::BACK_EDGES] == tier.back_edge_threshold / 2
        && machine.code.cdr().element(TIER1) == nil()
    {
        profile[profile::RECORDING] = 1;
        state.feedback = values::ints_mut(machine.code.cdr().element(FEEDBACK)).as_mut_ptr();
    }
    if profile[profile::BACK_EDGES] < tier.back_edge_threshold {
        return BackEdge::Continue(pc);
    }
    profile[profile::BACK_EDGES] = 0;
    let exit = (tier.back_edge)(machine, pc);
    // The tier may have compiled the body or moved its entry count.
    values::ints_mut(machine.code.cdr().element(PROFILE))[profile::NEXT] = i32::MIN;
    match exit {
        Some(TierExit::Finished(result)) => BackEdge::Finished(result),
        Some(TierExit::Resume {
            pc,
            pending: Some(unwind),
        }) => BackEdge::Unwound { pc, unwind },
        Some(TierExit::Resume { pc, pending: None }) => BackEdge::Continue(pc),
        None => BackEdge::Continue(pc),
    }
}

/// The value of `left && right` (or `||`), given that `left` did not decide it.
pub fn logic_result(is_and: bool, left: i32, right: i32) -> i32 {
    if is_and {
        if right == 0 {
            0
        } else if left == NA_LOGICAL || right == NA_LOGICAL {
            NA_LOGICAL
        } else {
            1
        }
    } else if right == 1 {
        1
    } else if left == NA_LOGICAL || right == NA_LOGICAL {
        NA_LOGICAL
    } else {
        0
    }
}

/// `x[[i]] <- value`, `x[i] <- value` or `x$name <- value` done in place, without
/// `complex_assign`'s replacement calls, when the result is certain to equal what the replacement
/// function would return: `x` is a local, unshared, classless vector, `i` a scalar position inside
/// it (or a name `x` has, for `$`), and `value` either any non-`NULL` element of a list under
/// `[[` or `$`, or an attribute-free scalar of `x`'s own atomic type. Returns whether it did the
/// assignment; otherwise the variable is unchanged.
unsafe fn assign_element_in_place(
    env: Sexp,
    variable: Sexp,
    index: Sexp,
    value: Sexp,
    kind: ElementKind,
) -> R<bool> {
    let Some(cell) = crate::heap().frame_cell(env, variable) else {
        return Ok(false);
    };
    if cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED) {
        return Ok(false);
    }
    let current = cell.cdr();
    // An argument's forced promise holds a value the caller may also hold, so the update goes
    // to a copy bound in place of the promise, as GNU R does for any value read through one.
    let through_promise = current.kind() == SexpType::Promise;
    let target = if through_promise {
        current.car()
    } else {
        current
    };
    if target == values::unbound() {
        return Ok(false);
    }
    let Some(result) = assign_element(target, index, value, kind, |target| {
        !through_promise && !values::maybe_shared(target) && eval::reuse_permitted(env, target)
    })?
    else {
        return Ok(false);
    };
    if result != current {
        eval::assign_local(variable, result, env, Some(cell))?;
    }
    cell.set_gp_bits(gp::BINDING_MISSING | gp::BINDING_ABSENT, false);
    Ok(true)
}

/// The update [`assign_element_in_place`] does on the value `target` a variable holds: the
/// variable's new value, which is `target` itself when `owned` says nothing else can see the
/// change, and otherwise a copy (or, for an appended name, a longer vector) the caller binds.
/// `None` declines to `complex_assign`.
unsafe fn assign_element(
    target: Sexp,
    index: Sexp,
    value: Sexp,
    kind: ElementKind,
    owned: impl FnOnce(Sexp) -> bool,
) -> R<Option<Sexp>> {
    let heap = crate::heap();
    if target == value
        || target.kind() == SexpType::Promise
        || target.is_object()
        || target.is_array()
        || target.is_altrep()
        || index.is_altrep()
        || values::has_attributes(index)
        || length(index) != 1
    {
        return Ok(None);
    }
    let fits = match target.kind() {
        SexpType::List => kind != ElementKind::Single && value != nil(),
        SexpType::Real | SexpType::Integer | SexpType::Logical | SexpType::String => {
            // A value of a lower atomic type converts to the target's, as `[<-` would.
            let converts = matches!(
                (target.kind(), value.kind()),
                (SexpType::Real, SexpType::Integer | SexpType::Logical)
                    | (SexpType::Integer, SexpType::Logical)
            );
            kind != ElementKind::Dollar
                && (value.kind() == target.kind() || converts)
                && length(value) == 1
                && !value.is_altrep()
                && !values::has_attributes(value)
        }
        _ => false,
    };
    if !fits {
        return Ok(None);
    }
    let position = match index.kind() {
        // `$<-` on a list replaces the first element whose name matches exactly; a name `x`
        // lacks appends an element, which `complex_assign` does.
        SexpType::String => {
            // An empty or `NA` name matches no element.
            let wanted = index.element(0);
            let names = values::get_attr(target, symbols().names);
            if wanted == values::na_string()
                || wanted.bytes().is_empty()
                || !(names == nil() || names.kind() == SexpType::String && !names.is_altrep())
            {
                return Ok(None);
            }
            let wanted_bytes = wanted.bytes();
            let found = (0..length(names)).find(|&at| {
                let name = names.element(at);
                name == wanted || (name != values::na_string() && name.bytes() == wanted_bytes)
            });
            match found {
                Some(at) => at as f64 + 1.0,
                // Appending a name: `x` grows in place where it has room when nothing else sees
                // it, as the replacement call would extend `x` itself, and is otherwise copied
                // with room to grow, so repeated appends are amortized.
                None => {
                    let attributes = target.attrib();
                    if attributes != nil()
                        && (attributes.tag() != symbols().names || attributes.cdr() != nil())
                    {
                        return Ok(None);
                    }
                    // `append_named` copies the value's payload, so a lower type converts
                    // to the target's first.
                    let value = match (target.kind(), value.kind()) {
                        (SexpType::Real, SexpType::Integer | SexpType::Logical) => {
                            let integer = value.slice::<i32>()[0];
                            let converted = values::scalar_real(if integer == NA_INTEGER {
                                NA_REAL
                            } else {
                                f64::from(integer)
                            })?;
                            heap.root(converted);
                            converted
                        }
                        (SexpType::Integer, SexpType::Logical) => {
                            let converted = values::scalar_integer(value.slice::<i32>()[0])?;
                            heap.root(converted);
                            converted
                        }
                        _ => value,
                    };
                    moves::modifying(target)?;
                    let reuse = owned(target);
                    return crate::subset::append_named(target, wanted, value, reuse).map(Some);
                }
            }
        }
        SexpType::Integer if index.slice::<i32>()[0] != NA_INTEGER => {
            f64::from(index.slice::<i32>()[0])
        }
        SexpType::Real => index.slice::<f64>()[0],
        _ => return Ok(None),
    };
    let count = length(target);
    // Assigning one past the end of a vector without attributes appends, in place where a
    // previous growth left room, and otherwise into a vector with room to grow, so repeated
    // appends are amortized (see `subset::grow`).
    let appends = position == count as f64 + 1.0 && target.attrib() == nil();
    // NaN fails both comparisons, and `x[[2.7]]` truncates to the second element.
    if !appends && !(position >= 1.0 && position < count as f64 + 1.0) {
        return Ok(None);
    }
    let position = position as usize - 1;
    // The conditions under which `complex_assign` updates the variable's value in place.
    // Otherwise the replacement function would change a shallow copy and bind it, which this
    // does without building the call.
    moves::modifying(target)?;
    let owned = owned(target);
    let target = if appends {
        // Of `subset::grows_in_place`'s tests, `owned` settles sharing and `appends` the
        // attributes, and an ALTREP vector is never resizable, so only the room is left.
        if owned && heap.is_resizable(target) && target.capacity() > count as isize {
            heap.resize_vector(target, count as isize + 1)?;
            target
        } else {
            crate::subset::append_copy(target, owned)?
        }
    } else if owned {
        target
    } else {
        let copy = values::shallow_duplicate(target)?;
        heap.root(copy);
        copy
    };
    match (target.kind(), value.kind()) {
        (SexpType::Real, SexpType::Real) => {
            values::reals_mut(target)[position] = value.slice::<f64>()[0]
        }
        (SexpType::Real, _) => {
            let integer = value.slice::<i32>()[0];
            values::reals_mut(target)[position] = if integer == NA_INTEGER {
                NA_REAL
            } else {
                f64::from(integer)
            };
        }
        (SexpType::Integer | SexpType::Logical, _) => {
            values::ints_mut(target)[position] = value.slice::<i32>()[0]
        }
        (SexpType::String, _) => heap.set_element(target, position, value.element(0)),
        _ => heap.set_element(target, position, value),
    }
    Ok(Some(target))
}

/// The arguments after the variable of the one-level assignment target `target`, as compiled
/// code evaluated them into `registers`, for [`eval::complex_assign`]: each a forced promise of
/// the argument's code, or the code itself where the register holds it (a formal left for
/// lookup, an empty argument or a constant), so no argument is evaluated a second time.
unsafe fn evaluated_arguments(target: Sexp, registers: &[Sexp]) -> R<SmallVec<[Sexp; 4]>> {
    let heap = crate::heap();
    let mut arguments = SmallVec::new();
    let mut rest = target.cdr().cdr();
    for &register in registers {
        let code = rest.car();
        arguments.push(if register == code {
            code
        } else {
            let promise = eval::forced_promise(code, register)?;
            heap.root(promise);
            promise
        });
        rest = rest.cdr();
    }
    Ok(arguments)
}

/// The value of a compiled assignment's subscript register. A register holding a symbol holds a
/// formal the compiler left for this lookup, and `None` means the formal is missing.
unsafe fn subscript_value(env: Sexp, index: Sexp) -> R<Option<Sexp>> {
    if index.kind() != SexpType::Symbol {
        return Ok(Some(index));
    }
    if eval::argument_is_missing(index, env)? {
        return Ok(None);
    }
    let value = eval::eval_symbol(index, env)?;
    crate::heap().root(value);
    Ok(Some(value))
}

/// Where [`replace_variable`]'s variable lives: its binding cell in the frame's environment, or
/// a slot of a frame that has none.
#[derive(Clone, Copy)]
enum Home {
    Cell(Sexp),
    Slot(*mut Sexp),
}

/// [`Op::Replace`]'s one-level update of a local variable, as `complex_assign` runs it, through
/// a builtin replacement or a closure that takes only the target and the value: `false`
/// leaves the assignment to `complex_assign`. `args` are the call's argument registers, the
/// first for the variable's value and the last holding the value assigned.
///
/// A variable in a slot (`env` is `NULL`) is updated without making the frame's environment
/// when the replacement is a builtin and the target is not an object, since then nothing can
/// look at the frame. Its value changes in place only when the slot alone holds it, which keeps
/// `y <- x; names(x) <- v` from changing `y`.
unsafe fn replace_variable(
    state: &Activation<'_, '_>,
    env: Sexp,
    variable: Sexp,
    (setter, lookup): (Sexp, usize),
    setter_call: Sexp,
    whole: Sexp,
    args: &mut [Sexp],
) -> R<bool> {
    let heap = crate::heap();
    let home = if env == nil() {
        match slot_named(state, variable) {
            Some(slot) => Home::Slot(slot),
            None => return Ok(false),
        }
    } else {
        match heap.frame_cell(env, variable) {
            Some(cell) if !cell.has_gp(gp::BINDING_LOCKED | gp::ACTIVE_BINDING) => Home::Cell(cell),
            _ => return Ok(false),
        }
    };
    let held = match home {
        Home::Cell(cell) => cell.cdr(),
        Home::Slot(slot) => *slot,
    };
    let mut target = if held.kind() == SexpType::Promise {
        held.car()
    } else {
        held
    };
    if target == unbound() || target == missing_arg() {
        return Ok(false);
    }
    // `complex_assign` reads the variable before it looks the replacement up, so a missing
    // variable is the error both report.
    let function = match home {
        Home::Cell(_) => lookup_function(setter, env, state.cache, lookup, unbound())?,
        Home::Slot(_) => slot_function(state, setter, lookup, unbound())?,
    };
    // Looking the replacement up may have forced a promise that gave the frame its environment.
    if matches!(home, Home::Slot(_)) && *state.header != nil() {
        return Ok(false);
    }
    let builtin = match function.kind() {
        // An object may dispatch to a replacement method, which `complex_assign` calls with the
        // `*tmp*` and value promises it expects.
        SexpType::Builtin => {
            if target.is_object() {
                return Ok(false);
            }
            Some(eval::builtin_def(function)?)
        }
        // A special such as `[<-` evaluates its own arguments, so only its direct entry can take
        // the registers; anything that entry declines goes to `complex_assign`.
        SexpType::Special => {
            let definition = eval::builtin_def(function)?;
            if target.is_object()
                || definition.direct.is_none()
                || !replacement_arguments(setter_call, true)
            {
                return Ok(false);
            }
            Some(definition)
        }
        // A closure's frame has the caller's as its parent frame, which must then exist.
        SexpType::Closure if args.len() == 2 && matches!(home, Home::Cell(_)) => None,
        _ => return Ok(false),
    };
    let special = function.kind() == SexpType::Special;
    moves::modifying(target)?;
    // Registers count no reference, so an argument that is the target itself, as in
    // `names(x) <- x`, must not watch the replacement change it.
    let shared = match home {
        Home::Cell(_) => values::maybe_shared(target) || !eval::reuse_permitted(env, target),
        Home::Slot(_) => {
            // The first argument register still holds the value the previous run of this
            // instruction replaced, which is no reason to copy.
            args[0] = nil();
            held != target || !slot_owns(state, target)
        }
    };
    if shared || args[1..].contains(&target) {
        let was_array = target.is_array();
        target = values::shallow_duplicate_attr(target)?;
        target.set_array(was_array);
    }
    heap.root(target);
    args[0] = target;
    if let Home::Cell(_) = home {
        interp().tmp_values.push((env, target));
    }
    let direct = match builtin {
        Some(definition)
            if definition.direct.is_some() && replacement_arguments(setter_call, special) =>
        {
            eval::call_direct(definition, setter_call, args)
        }
        _ => Ok(None),
    };
    if special && matches!(direct, Ok(None)) {
        if let Home::Cell(_) = home {
            interp().tmp_values.pop();
        }
        return Ok(false);
    }
    if let Home::Cell(_) = home {
        heap.count_transient_binding(env, symbols().tmp);
    }
    let result = match (direct, builtin) {
        (Ok(Some(value)), _) => Ok(value),
        (Err(unwind), _) => Err(unwind),
        (Ok(None), Some(definition)) => {
            let mut arguments = eval::Args::new();
            let mut item = setter_call.cdr();
            for &value in args.iter() {
                arguments.push(Arg {
                    tag: item.tag(),
                    value,
                });
                item = item.cdr();
            }
            let interpreter = interp();
            let outer =
                interpreter
                    .assignment_call
                    .replace((setter_call, whole, interpreter.frames.len()));
            let result = eval::call_builtin(definition, setter_call, function, &arguments, env);
            interp().assignment_call = outer;
            result
        }
        (Ok(None), None) => replace_with_closure(function, setter, whole, target, args[1], env),
    };
    if let Home::Cell(_) = home {
        interp().tmp_values.pop();
    }
    let result = result?;
    let bound = match home {
        Home::Cell(cell) => cell.cdr() == result,
        Home::Slot(slot) => *slot == result,
    };
    let result = values::unwrap_assigned(result, bound);
    match home {
        Home::Cell(cell) => eval::assign_local(variable, result, env, Some(cell))?,
        // A warning's calling handler may have looked at the frame, which then holds the
        // variable in its environment instead of the slot.
        Home::Slot(_) if *state.header != nil() => {
            let env = *state.header;
            heap.root(result);
            eval::assign_local(variable, result, env, heap.frame_cell(env, variable))?
        }
        Home::Slot(slot) => heap.store_slot(&mut *slot, result),
    }
    Ok(true)
}

/// Whether a replacement call's arguments are untagged but for the trailing `value`, the shape
/// `complex_assign` builds and a [`BuiltinDef::direct`] entry takes in formal order. Only a
/// special's entry, which sees subscripts as `[<-` does, takes an empty one (`x[i, ]`).
unsafe fn replacement_arguments(setter_call: Sexp, empty_allowed: bool) -> bool {
    let mut cell = setter_call.cdr();
    while cell != nil() {
        let last = cell.cdr() == nil();
        let expected = if last { symbols().value } else { nil() };
        if cell.tag() != expected || (!empty_allowed && cell.car() == missing_arg()) {
            return false;
        }
        cell = cell.cdr();
    }
    true
}

/// Calls the replacement closure `function` as `` `f<-`(`*tmp*`, value = <promise>) ``, with
/// the promises `complex_assign` passes: neither counts a reference, so a replacement that
/// updates its target in place leaves it unshared for the next update.
unsafe fn replace_with_closure(
    function: Sexp,
    setter: Sexp,
    whole: Sexp,
    target: Sexp,
    value: Sexp,
    env: Sexp,
) -> R<Sexp> {
    let heap = crate::heap();
    let tmp = symbols().tmp;
    let object = eval::forced_promise(tmp, target)?;
    heap.untrack_references(object);
    heap.root(object);
    let value = eval::forced_promise(whole.cdr().cdr().car(), value)?;
    heap.untrack_references(value);
    heap.root(value);
    let call = values::pairlist(
        &[(nil(), setter), (nil(), tmp), (symbols().value, value)],
        SexpType::Language,
    )?;
    heap.root(call);
    let arguments = values::pairlist(
        &[(nil(), object), (symbols().value, value)],
        SexpType::Pairlist,
    )?;
    heap.root(arguments);
    eval::apply_call(call, function, arguments, env)
}

/// `variable[row, column] <- value` on an atomic matrix, done in place when the matrix is the
/// frame's own unshared value, `value` is one element of its type and both subscripts are in
/// bounds. Returns false, having changed nothing, when the general assignment must run.
unsafe fn assign_matrix_element_in_place(
    env: Sexp,
    variable: Sexp,
    row: Sexp,
    column: Sexp,
    value: Sexp,
) -> R<bool> {
    let heap = crate::heap();
    let Some(cell) = heap.frame_cell(env, variable) else {
        return Ok(false);
    };
    let mut target = cell.cdr();
    // An argument's forced promise holds a value the caller may also hold, which the
    // replacement copies and binds in place of the promise, as GNU R does for any value it
    // reads through a promise.
    let through_promise = target.kind() == SexpType::Promise;
    if through_promise {
        target = target.car();
        if target == values::unbound() {
            return Ok(false);
        }
    }
    if cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED)
        || target == value
        || target.is_object()
        || target.is_altrep()
        || !matches!(
            target.kind(),
            SexpType::Real | SexpType::Integer | SexpType::Logical | SexpType::String
        )
        || value.kind() != target.kind()
        || length(value) != 1
        || value.is_altrep()
        || values::has_attributes(value)
    {
        return Ok(false);
    }
    let dim = values::get_attr(target, symbols().dim);
    if dim.kind() != SexpType::Integer || length(dim) != 2 {
        return Ok(false);
    }
    let rows = dim.slice::<i32>()[0];
    let columns = dim.slice::<i32>()[1];
    let Some(row) = matrix_subscript(row, rows) else {
        return Ok(false);
    };
    let Some(column) = matrix_subscript(column, columns) else {
        return Ok(false);
    };
    let position = row + column * rows as usize;
    // The conditions under which `complex_assign` updates the variable's value in place.
    moves::modifying(target)?;
    if values::maybe_shared(target) || !eval::reuse_permitted(env, target) {
        return Ok(false);
    }
    match target.kind() {
        SexpType::Real => values::reals_mut(target)[position] = value.slice::<f64>()[0],
        SexpType::Integer | SexpType::Logical => {
            values::ints_mut(target)[position] = value.slice::<i32>()[0]
        }
        _ => heap.set_element(target, position, value.element(0)),
    }
    cell.set_gp_bits(gp::BINDING_MISSING | gp::BINDING_ABSENT, false);
    Ok(true)
}

/// The zero-based position a one-element numeric matrix subscript selects in a dimension of
/// `extent`, or `None` when it is out of bounds, NA, or anything but a plain number.
unsafe fn matrix_subscript(index: Sexp, extent: i32) -> Option<usize> {
    if index.is_altrep() || values::has_attributes(index) || length(index) != 1 {
        return None;
    }
    let position = match index.kind() {
        SexpType::Integer if index.slice::<i32>()[0] != NA_INTEGER => {
            f64::from(index.slice::<i32>()[0])
        }
        SexpType::Real => index.slice::<f64>()[0],
        _ => return None,
    };
    // NaN fails both comparisons, and `x[2.7, 1]` truncates to the second row.
    if !(position >= 1.0 && position < f64::from(extent) + 1.0) {
        return None;
    }
    Some(position as usize - 1)
}

pub(crate) unsafe fn call_builtin(
    definition: &'static BuiltinDef,
    call: Sexp,
    function: Sexp,
    args: &[Arg],
    env: Sexp,
) -> R<Sexp> {
    if definition.generic != Generic::None {
        // Dispatch hands the frame to a method, so an unreified slot frame (a `NULL` `env`)
        // gets its environment, but only when an argument is an object to dispatch on.
        let env = if env == nil() && args.iter().any(|arg| arg.value.is_object()) {
            eval::slot_owner_env(Reify::Site("builtin dispatch"))?
        } else {
            env
        };
        if let Some(value) = dispatch::try_builtin_dispatch(definition, call, function, args, env)?
        {
            return Ok(value);
        }
    }
    eval::call_builtin(definition, call, function, args, env)
}

unsafe fn record_feedback(constants: Sexp, pc: usize, value: Sexp) {
    let feedback = constants.element(FEEDBACK);
    values::ints_mut(feedback)[pc] |= value_kind(value) as i32;
}

/// Arithmetic and comparison on two attribute-free scalar doubles or integers. `None` sends the
/// operation to the builtin, which handles everything else (and the warnings).
unsafe fn fast_arithmetic(operator: Arith, left: Sexp, right: Sexp) -> R<Option<Sexp>> {
    let scalar = |value: Sexp| {
        matches!(value.kind(), SexpType::Real | SexpType::Integer)
            && value.length() == 1
            && !values::has_attributes(value)
            && !value.is_altrep()
            && !value.is_array()
    };
    if !scalar(left) || !scalar(right) {
        return Ok(None);
    }
    let both_integer = left.kind() == SexpType::Integer && right.kind() == SexpType::Integer;
    let as_real = |value: Sexp| match value.kind() {
        SexpType::Integer => {
            let integer = values::ints(value)[0];
            if integer == NA_INTEGER {
                NA_REAL
            } else {
                integer as f64
            }
        }
        _ => values::reals(value)[0],
    };
    let comparison = |result: Option<bool>| -> R<Option<Sexp>> {
        Ok(Some(values::scalar_logical(match result {
            None => NA_LOGICAL,
            Some(flag) => flag as i32,
        })?))
    };
    if both_integer {
        let (a, b) = (values::ints(left)[0], values::ints(right)[0]);
        let na = a == NA_INTEGER || b == NA_INTEGER;
        let integer = |result: Option<i32>| -> R<Option<Sexp>> {
            match result {
                Some(value) if value != NA_INTEGER => Ok(Some(values::scalar_integer(value)?)),
                _ => Ok(None),
            }
        };
        return match operator {
            Arith::Add if !na => integer(a.checked_add(b)),
            Arith::Subtract if !na => integer(a.checked_sub(b)),
            Arith::Multiply if !na => integer(a.checked_mul(b)),
            Arith::Equal => comparison((!na).then_some(a == b)),
            Arith::NotEqual => comparison((!na).then_some(a != b)),
            Arith::Less => comparison((!na).then_some(a < b)),
            Arith::Greater => comparison((!na).then_some(a > b)),
            Arith::LessEqual => comparison((!na).then_some(a <= b)),
            Arith::GreaterEqual => comparison((!na).then_some(a >= b)),
            Arith::Modulo => Ok(Some(values::scalar_integer(crate::arith::integer_modulo(
                a, b,
            ))?)),
            Arith::IntegerDivide => Ok(Some(values::scalar_integer(
                crate::arith::integer_divide(a, b),
            )?)),
            _ => Ok(None),
        };
    }
    let (a, b) = (as_real(left), as_real(right));
    let real = |value: f64| -> R<Option<Sexp>> { Ok(Some(values::scalar_real(value)?)) };
    let nan = a.is_nan() || b.is_nan();
    match operator {
        Arith::Add => real(crate::arith::add(a, b)),
        Arith::Subtract => real(a - b),
        Arith::Multiply => real(crate::arith::multiply(a, b)),
        Arith::Divide => real(a / b),
        // R_pow's special cases (1^y, x^0, NA handling) differ from powf; the builtin has them.
        Arith::Power => {
            if b == 2.0 && !is_na_real(a) {
                real(a * a)
            } else {
                Ok(None)
            }
        }
        Arith::Equal => comparison((!nan).then_some(a == b)),
        Arith::NotEqual => comparison((!nan).then_some(a != b)),
        Arith::Less => comparison((!nan).then_some(a < b)),
        Arith::Greater => comparison((!nan).then_some(a > b)),
        Arith::LessEqual => comparison((!nan).then_some(a <= b)),
        Arith::GreaterEqual => comparison((!nan).then_some(a >= b)),
        // The builtin warns when the remainder loses all accuracy.
        Arith::Modulo if !crate::arith::modulus_loses_accuracy(a, b) => {
            real(crate::arith::real_modulo(a, b))
        }
        Arith::Modulo => Ok(None),
        Arith::IntegerDivide => real(crate::arith::real_integer_divide(a, b)),
    }
}

// Inline caches.

/// A variable's value: the frame's own binding, or the binding the cache remembers in the
/// enclosing environments, or a full lookup that refills the cache.
unsafe fn lookup_variable(
    symbol: Sexp,
    env: Sexp,
    cache: LookupCache,
    slot: usize,
    held: Range<usize>,
) -> R<Sexp> {
    let heap = crate::heap();
    let hint = &mut *cache.hints.add(slot);
    if let Some(cell) = heap.frame_cell_hinted(env, symbol, hint) {
        let value = cell.cdr();
        if cell.has_gp(gp::ACTIVE_BINDING) || value == missing_arg() || value == unbound() {
            return holding(held, || eval::eval_symbol(symbol, env));
        }
        if value.kind() == SexpType::Promise {
            // An argument: force it here rather than repeat the frame search `eval_symbol` does.
            if value.car() != unbound() {
                return Ok(value.car());
            }
            let forced = holding(held, || eval::force_promise(value))?;
            heap.root(forced);
            return Ok(forced);
        }
        return Ok(value);
    }
    lookup_outside(
        symbol,
        env.enclosure(),
        LexicalFrame::Env(env),
        cache,
        slot,
        held,
    )
}

/// The frame a variable lookup runs on behalf of. A lookup that continues past the frame's own
/// bindings starts in the enclosure, but an unbound variable is still reported against the
/// frame's call, so the frame travels with the lookup.
#[derive(Clone, Copy)]
enum LexicalFrame {
    Env(Sexp),
    /// A slot frame without an environment, by its activation's header register.
    Slots(usize),
}

impl LexicalFrame {
    unsafe fn of(machine: &Machine<'_>) -> LexicalFrame {
        match machine.env() {
            env if env == nil() => LexicalFrame::Slots(machine.header),
            env => LexicalFrame::Env(env),
        }
    }

    unsafe fn call(self) -> Sexp {
        match self {
            LexicalFrame::Env(env) => eval::lexical_call(env),
            LexicalFrame::Slots(header) => interp()
                .frames
                .iter()
                .rev()
                .find(|frame| frame.slots as usize == header)
                .map_or(nil(), |frame| frame.call),
        }
    }
}

/// [`lookup_variable`] past a frame that does not bind `symbol`: the lookup from the frame's
/// enclosure `start`. Inlined into both callers: as a call, a free variable's read in a heap
/// frame cost about 14 more instructions in saved registers.
#[inline(always)]
unsafe fn lookup_outside(
    symbol: Sexp,
    start: Sexp,
    lexical: LexicalFrame,
    cache: LookupCache,
    slot: usize,
    held: Range<usize>,
) -> R<Sexp> {
    let hint = &mut *cache.hints.add(slot);
    let env = start;
    let enclosure = match outside_function_frames(symbol, start, hint) {
        Ok(enclosure) => enclosure,
        Err(cell) => {
            let value = cell.cdr();
            if value.kind() == SexpType::Promise && value.car() != unbound() {
                return Ok(value.car());
            }
            if cell.has_gp(gp::ACTIVE_BINDING)
                || value == missing_arg()
                || value == unbound()
                || value.kind() == SexpType::Promise
            {
                return holding(held, || {
                    eval::eval_symbol_reporting(symbol, env, || lexical.call())
                });
            }
            return Ok(value);
        }
    };
    if let Some(cell) = cached_cell(symbol, enclosure, cache, slot)
        && !cell.has_gp(gp::ACTIVE_BINDING)
    {
        let value = cell.cdr();
        if value.kind() == SexpType::Promise {
            if value.car() != unbound() {
                return Ok(value.car());
            }
            return holding(held, || {
                eval::eval_symbol_reporting(symbol, env, || lexical.call())
            });
        }
        if value != missing_arg() && value != unbound() {
            return Ok(value);
        }
    }
    activity::count(Counter::LookupMisses);
    let _activity = activity::enter(Activity::Lookup, 0);
    if let Some(cell) = resolve(symbol, enclosure, false)? {
        fill_cache(symbol, enclosure, cell, cache, slot);
    }
    holding(held, || {
        eval::eval_symbol_reporting(symbol, env, || lexical.call())
    })
}

/// Runs `run`, which forces a promise or an active binding and so runs code. The registers
/// below the destination hold values of the expression being evaluated without counting as
/// references, so they count as referenced while that code runs: in `x + y` where forcing `y`
/// runs `x[] <- 1`, the assignment must copy `x` rather than change the left operand.
unsafe fn holding(held: Range<usize>, run: impl FnOnce() -> R<Sexp>) -> R<Sexp> {
    if held.is_empty() {
        return run();
    }
    // The held registers belong to the running activation, and the code `run` starts uses only
    // registers above the file's current length, so they read the same before and after.
    let registers = &interp().bytecode.registers;
    let Some(values) = registers.get(held.clone()) else {
        return run();
    };
    for value in values {
        value.increment_refcount();
    }
    let result = run();
    for value in interp().bytecode.registers.get(held).unwrap_or_default() {
        value.decrement_refcount();
    }
    result
}

/// A function's value, with GNU R's rule that bindings to non-functions are skipped.
unsafe fn lookup_function(
    symbol: Sexp,
    env: Sexp,
    cache: LookupCache,
    slot: usize,
    call: Sexp,
) -> R<Sexp> {
    let heap = crate::heap();
    let hint = &mut *cache.hints.add(slot);
    if heap.frame_cell_hinted(env, symbol, hint).is_some() {
        return eval::find_called_fun(symbol, env, call);
    }
    let Ok(enclosure) = outside_function_frames(symbol, env.enclosure(), hint) else {
        return eval::find_called_fun(symbol, env, call);
    };
    lookup_function_outside(symbol, env, enclosure, cache, slot, call)
}

/// [`lookup_function`] from `enclosure`, the first environment outward from `env` that is not
/// a function frame, when nothing before it binds `symbol`. The cache hit is inlined into every
/// function lookup: it is two loads and a compare, and a call to it cost about as much again in
/// saved registers. A cached cell is never active (see [`op_guard`]).
#[inline(always)]
unsafe fn lookup_function_outside(
    symbol: Sexp,
    env: Sexp,
    enclosure: Sexp,
    cache: LookupCache,
    slot: usize,
    call: Sexp,
) -> R<Sexp> {
    if let Some(cell) = cached_cell(symbol, enclosure, cache, slot) {
        let mut value = cell.cdr();
        if value.kind() == SexpType::Promise && value.car() != unbound() {
            value = value.car();
        }
        if values::is_function(value) {
            return Ok(value);
        }
    }
    lookup_function_missed(symbol, env, enclosure, cache, slot, call)
}

#[cold]
#[inline(never)]
unsafe fn lookup_function_missed(
    symbol: Sexp,
    env: Sexp,
    enclosure: Sexp,
    cache: LookupCache,
    slot: usize,
    call: Sexp,
) -> R<Sexp> {
    activity::count(Counter::LookupMisses);
    // The frames between `env` and `enclosure` do not bind `symbol`, so the cell `resolve`
    // finds, which holds a function (forced, if it was a promise), is the answer.
    if let Some(cell) = resolve(symbol, enclosure, true)? {
        fill_cache(symbol, enclosure, cell, cache, slot);
        let mut value = cell.cdr();
        if value.kind() == SexpType::Promise {
            value = value.car();
        }
        if values::is_function(value) {
            return Ok(value);
        }
    }
    eval::find_called_fun(symbol, env, call)
}

/// [`lookup_function`] for a symbol the frame cannot bind, walking from the frame's `enclosure`
/// so the frame itself is neither needed nor probed. The cache is filled under the same first
/// environment that is not a function frame as the guard's fast path reads it under.
#[inline(always)]
unsafe fn lookup_enclosing_function(
    symbol: Sexp,
    enclosure: Sexp,
    cache: LookupCache,
    slot: usize,
) -> R<Sexp> {
    match outside_function_frames(symbol, enclosure, &mut *cache.hints.add(slot)) {
        Ok(outside) => lookup_function_outside(symbol, enclosure, outside, cache, slot, unbound()),
        Err(_) => eval::find_called_fun(symbol, enclosure, unbound()),
    }
}

/// The first environment from `env` outward that is not a function frame, or the cell of the
/// function frame before it that binds `symbol`. A function frame gains and loses bindings
/// without moving the symbol's lookup epoch, so the lookup cache cannot cover it and it is
/// probed on every lookup; for a frame that does not bind the symbol, which is the usual case,
/// the frame's symbol filter answers with one load. A closure created inside a call, such as
/// the `FUN` that `pmin` passes to `vapply`, encloses a fresh frame per call, so caching only
/// from the first environment that is not a function frame is what lets its lookups of base
/// functions hit the cache at all.
#[inline(always)]
unsafe fn outside_function_frames(symbol: Sexp, env: Sexp, hint: &mut u32) -> Result<Sexp, Sexp> {
    if !env.has_gp(gp::FUNCTION_FRAME) {
        return Ok(env);
    }
    walk_function_frames(symbol, env, hint)
}

#[inline(never)]
unsafe fn walk_function_frames(symbol: Sexp, mut env: Sexp, hint: &mut u32) -> Result<Sexp, Sexp> {
    let heap = crate::heap();
    while env.has_gp(gp::FUNCTION_FRAME) {
        if let Some(cell) = heap.frame_cell_hinted(env, symbol, hint) {
            return Err(cell);
        }
        env = env.enclosure();
    }
    Ok(env)
}

/// A code object's lookup caches: per lookup slot, the stamp it was filled under (in the
/// `EPOCHS` constant), an (enclosure, cell) pair (in the code's tag), and the position hint of
/// `Heap::frame_cell_hinted` (in the `HINTS` constant). The vectors are allocated with the
/// code object and never resized, so their storage is addressed directly.
#[derive(Clone, Copy)]
struct LookupCache {
    /// Read as the stamps' bits, which compare in one instruction where a conversion to `f64`
    /// took several.
    epochs: *mut u64,
    caches: Sexp,
    slots: *const Sexp,
    /// Per lookup slot, where the binding sat in the frame that last bound the symbol.
    hints: *mut u32,
}

impl LookupCache {
    unsafe fn of(code: Sexp) -> Self {
        let caches = code.tag();
        let constants = code.cdr();
        LookupCache {
            epochs: pool_entry(constants, EPOCHS).data().cast::<u64>(),
            caches,
            slots: caches.data() as *const Sexp,
            hints: pool_ints(constants, HINTS).cast::<u32>(),
        }
    }
}

/// One of a code object's inline caches, used whole for something other than a lookup: a key
/// held weakly, a value, and a stamp of the caller's choosing.
#[derive(Clone, Copy)]
pub(crate) struct CacheSlot {
    cache: LookupCache,
    slot: usize,
}

impl CacheSlot {
    pub(crate) unsafe fn of(code: Sexp, slot: usize) -> Self {
        CacheSlot {
            cache: LookupCache::of(code),
            slot,
        }
    }

    pub(crate) unsafe fn key(self) -> Sexp {
        *self.cache.slots.add(self.slot * 2)
    }

    pub(crate) unsafe fn value(self) -> Sexp {
        *self.cache.slots.add(self.slot * 2 + 1)
    }

    pub(crate) unsafe fn stamp(self) -> u64 {
        *self.cache.epochs.add(self.slot)
    }

    pub(crate) unsafe fn set_stamp(self, stamp: u64) {
        *self.cache.epochs.add(self.slot) = stamp;
    }

    pub(crate) unsafe fn fill(self, key: Sexp, value: Sexp, stamp: u64) {
        let heap = crate::heap();
        heap.set_element(self.cache.caches, self.slot * 2, key);
        heap.set_element(self.cache.caches, self.slot * 2 + 1, value);
        self.set_stamp(stamp);
    }
}

#[inline(always)]
unsafe fn cached_cell(
    symbol: Sexp,
    enclosure: Sexp,
    cache: LookupCache,
    slot: usize,
) -> Option<Sexp> {
    if *cache.epochs.add(slot) != crate::heap().lookup_stamp(symbol)
        || *cache.slots.add(slot * 2) != enclosure
    {
        return None;
    }
    Some(*cache.slots.add(slot * 2 + 1))
}

unsafe fn fill_cache(symbol: Sexp, enclosure: Sexp, cell: Sexp, cache: LookupCache, slot: usize) {
    let heap = crate::heap();
    heap.observe_environment(enclosure);
    heap.set_element(cache.caches, slot * 2, enclosure);
    heap.set_element(cache.caches, slot * 2 + 1, cell);
    *cache.epochs.add(slot) = heap.lookup_stamp(symbol);
}

/// The cell a lookup from `enclosure` finds, when the result can be cached: the walk passes no
/// function frame (whose bindings come and go without moving the epoch) and, for a function
/// lookup, skips no binding (which could later start holding a function).
unsafe fn resolve(symbol: Sexp, mut env: Sexp, function: bool) -> R<Option<Sexp>> {
    let heap = crate::heap();
    let empty = heap.known().empty_env;
    while env != empty {
        if env.has_gp(gp::FUNCTION_FRAME) {
            return Ok(None);
        }
        let cell = if env == heap.known().global_env {
            match heap.global_cell(symbol) {
                Some((cell, true)) => Some(cell),
                _ => return Ok(None),
            }
        } else {
            heap.frame_cell(env, symbol)
        };
        if let Some(cell) = cell {
            if cell.has_gp(gp::ACTIVE_BINDING) {
                return Ok(None);
            }
            if !function {
                return Ok(Some(cell));
            }
            let mut value = cell.cdr();
            if value.kind() == SexpType::Promise {
                value = eval::force_promise(value)?;
            }
            return Ok(values::is_function(value).then_some(cell));
        }
        env = env.enclosure();
    }
    Ok(None)
}

// The `compiler` package's internals.

/// `mkCode(code, consts)`: the source expression, which `compiler` stores as the first constant.
unsafe fn do_mk_code(call: &Call<'_>) -> R<Sexp> {
    let constants = call.arg(1);
    if constants.kind() != SexpType::List || length(constants) == 0 {
        return Err(call.error("constant buffer must be a generic vector"));
    }
    Ok(constants.element(0))
}

/// `growconst(buffer)`: the constant buffer at twice its length.
unsafe fn do_grow_constants(call: &Call<'_>) -> R<Sexp> {
    let buffer = constant_buffer(call)?;
    let count = length(buffer);
    let grown = values::alloc(SexpType::List, 2 * count.max(1))?;
    for index in 0..count {
        values::set_elt(grown, index, buffer.element(index));
    }
    Ok(grown)
}

/// `putconst(buffer, count, x)`: the 0-based index of `x` among the first `count` constants,
/// adding it at `count` when no identical constant is there yet.
unsafe fn do_put_constant(call: &Call<'_>) -> R<Sexp> {
    let buffer = constant_buffer(call)?;
    let count = values::as_integer_scalar(call.arg(1));
    if count < 0 || count as usize >= length(buffer) {
        return Err(call.error("bad constCount value"));
    }
    let count = count as usize;
    let value = call.arg(2);
    let flags = crate::sort::IdenticalFlags::default();
    for index in 0..count {
        let constant = buffer.element(index);
        if constant == value || crate::sort::identical(constant, value, &flags) {
            return values::scalar_integer(index as i32);
        }
    }
    values::set_elt(buffer, count, value);
    values::scalar_integer(count as i32)
}

/// `getconst(buffer, n)`: the first `n` constants.
unsafe fn do_get_constants(call: &Call<'_>) -> R<Sexp> {
    let buffer = constant_buffer(call)?;
    let count = values::as_integer_scalar(call.arg(1));
    if count < 0 || count as usize > length(buffer) {
        return Err(call.error("bad n"));
    }
    let constants = values::alloc(SexpType::List, count as usize)?;
    for index in 0..count as usize {
        values::set_elt(constants, index, buffer.element(index));
    }
    Ok(constants)
}

unsafe fn constant_buffer(call: &Call<'_>) -> R<Sexp> {
    let buffer = call.arg(0);
    if buffer.kind() != SexpType::List {
        return Err(call.error("constant buffer must be a generic vector"));
    }
    Ok(buffer)
}
