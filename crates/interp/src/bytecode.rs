//! The tier-0 bytecode interpreter.
//!
//! A closure body compiles to bytecode on its second call, or takes its code from its package's
//! code image then. The code replaces the body as a
//! `BCODESXP`, as GNU R stores compiled closures, so it lives and dies with the closure and
//! everything that reads a body as source (`body()`, `deparse`, `identical`, serialization)
//! goes through [`closure_body`], which returns the expression.
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
//!   effect-free fact (see [`crate::effects`]), and the tenth the last argument-match plan of a
//!   closure body (see `eval::match_args_cached`).
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
        Interp,
        builtins::{Arg, BuiltinDef, Call, Generic, internal},
        conditions, dispatch, effects,
        eval::{self, SwitchSelector, as_condition},
        interp, moves,
        unwind::{Jump, R, Unwind, error, error_in},
        values::{self, length, missing_arg, nil, symbol_name, symbols, unbound},
    },
    heap::{
        Heap, Marker,
        activity::{self, Activity, Counter},
        object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Sexp, SexpType, gp, is_na_real},
    },
    rustc_hash::{FxHashMap, FxHashSet},
    smallvec::{SmallVec, smallvec},
    std::ops::Range,
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
        // A body the compiler rejects stays an expression, as in `run_body`.
        if let Ok(Some(code)) = compile(closure.cdr(), closure) {
            crate::heap().set_cdr(closure, code);
        }
        Ok(closure)
    }),
    internal("growconst", 1, do_grow_constants),
    internal("putconst", 3, do_put_constant),
    internal("getconst", 2, do_get_constants),
    internal("enableJIT", 1, |_| unsafe { values::scalar_integer(0) }),
    internal("compilePKGS", 1, |_| unsafe { values::scalar_logical(0) }),
];

/// The byte-code version GNU R 4.3 writes, which `compiler` stamps into every code buffer.
const GNU_R_BYTECODE_VERSION: i32 = 12;

/// The bytecode tier's session state: the register file and the call counts that decide when
/// a closure compiles.
pub struct State {
    /// Allocated once at [`REGISTER_FILE_CAPACITY`] and never grown past it, so a register
    /// window's address is stable for as long as the window exists.
    registers: Vec<Sexp>,
    /// Calls seen per uncompiled body. Keys are only compared, never dereferenced, so a body
    /// that died and whose address was reused merely compiles one call early.
    calls: FxHashMap<Sexp, u32>,
    tier: Option<Tier>,
}

impl Default for State {
    fn default() -> State {
        State {
            registers: Vec::with_capacity(REGISTER_FILE_CAPACITY),
            calls: FxHashMap::default(),
            tier: None,
        }
    }
}

impl State {
    pub(crate) unsafe fn trace(&self, marker: &mut Marker<'_>) {
        for &register in &self.registers {
            marker.mark(register);
        }
    }
}

/// The register file holds this many registers across all active frames. Deep recursion runs
/// out of C stack or `expressions` long before it runs out of registers.
const REGISTER_FILE_CAPACITY: usize = 1 << 21;

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
    /// Called on a backward jump to `target` once the code's back-edge counter crosses
    /// [`Tier::back_edge_threshold`]. Returns `None` to keep interpreting at `target`.
    pub back_edge: unsafe fn(&mut Machine<'_>, target: usize) -> Option<TierExit>,
    pub back_edge_threshold: i32,
    /// Called before a typed closure gets a frame, with its arguments matched by position or by
    /// exact name. Returns `None` to let the call proceed as usual, or the value a native
    /// function computed without a frame, promises or annotation checks.
    pub call: unsafe fn(function: Sexp, args: &[crate::builtins::Arg]) -> Option<R<Sexp>>,
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
    pub env: Sexp,
    /// Where the activation's register window starts in the register file.
    pub base: usize,
    /// The handle-stack mark to release to on a backward jump.
    pub mark: usize,
    pub loops: &'a mut [LoopState],
}

/// Installs the optimizing tier.
pub fn set_tier(tier: Option<Tier>) {
    interp().bytecode.tier = tier;
}

pub fn tier() -> Option<Tier> {
    interp().bytecode.tier
}

/// The address of register `index` of the file. Valid while the window that holds it exists.
pub fn register_address(index: usize) -> *mut Sexp {
    // The buffer never reallocates, so this pointer stays valid as the file grows and shrinks.
    unsafe { interp().bytecode.registers.as_mut_ptr().add(index) }
}

/// Grows the register file to `length` registers, filling new ones with `NULL`.
pub fn reserve_registers(length: usize) -> R<()> {
    let registers = &mut interp().bytecode.registers;
    if length > registers.capacity() {
        return Err(error(
            "evaluation nested too deeply: the register file is full",
        ));
    }
    if length > registers.len() {
        registers.resize(length, nil());
    }
    Ok(())
}

/// Runs a byte-code object that appears as an expression, which only happens when code reads a
/// compiled body directly and evaluates it.
pub unsafe fn execute(code: Sexp, env: Sexp) -> R<Sexp> {
    eval::eval(source_of(code), env)
}

/// Runs a closure's body, compiling it on its second call.
pub unsafe fn run_body(function: Sexp, body: Sexp, env: Sexp) -> R<Sexp> {
    if body.kind() == SexpType::Bytecode {
        return run(function, body, env);
    }
    if !worth_compiling(body) {
        return eval::eval(body, env);
    }
    let state = &mut interp().bytecode;
    let calls = state.calls.entry(body).or_insert(0);
    *calls += 1;
    // A body with a loop pays for compiling within its first call, as GNU R's JIT assumes, and
    // so does a typed one, whose signature lets the JIT compile it at that call too.
    if *calls < COMPILE_AFTER_CALLS
        && !contains_loop(body, LOOP_SEARCH_DEPTH)
        && !crate::annotations::is_typed(function)
    {
        if state.calls.len() > MAX_TRACKED_BODIES {
            state.calls.clear();
        }
        return eval::eval(body, env);
    }
    state.calls.remove(&body);
    // A package's code image replaces compiling, at the same call, rather than running code from
    // the first call: decoding costs about as much as compiling, and many closures run only once.
    if let Some(code) = crate::code_image::take_pending(function, body)? {
        return run(function, code, env);
    }
    // A body the compiler rejects (an error is its way of saying so, such as for a `break`
    // outside a loop) still runs correctly on the AST evaluator.
    match compile(body, function) {
        Ok(Some(code)) => {
            crate::heap().set_cdr(function, code);
            run(function, code, env)
        }
        Ok(None) | Err(_) => eval::eval(body, env),
    }
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
/// compile yet. A body the compiler cannot handle stays an expression.
pub unsafe fn compiled_body(function: Sexp) -> R<Sexp> {
    let body = function.cdr();
    if body.kind() == SexpType::Bytecode {
        return Ok(body);
    }
    if let Some(code) = crate::code_image::take_pending(function, body)? {
        return Ok(code);
    }
    match compile(body, function)? {
        Some(code) => {
            crate::heap().set_cdr(function, code);
            Ok(code)
        }
        None => Ok(body),
    }
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
                let compiled = compile(source, closure).ok().flatten();
                if let Some(compiled) = compiled {
                    crate::heap().set_element(constants, body, compiled);
                }
                compiled
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
pub const FIRST_CONSTANT: usize = 10;

/// The profile counters, positions in the `PROFILE` integer vector.
pub mod profile {
    /// How often the code started running.
    pub const ENTRIES: usize = 0;
    /// Backward jumps taken since the counter was last reset.
    pub const BACK_EDGES: usize = 1;
    /// The optimizing tier's state for this code, which only that tier interprets.
    pub const TIER_STATE: usize = 2;
    pub const LENGTH: usize = 3;
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

unsafe fn contains_loop(expression: Sexp, depth: u32) -> bool {
    if depth == 0 || expression.kind() != SexpType::Language {
        return false;
    }
    let head = expression.car();
    if head.kind() == SexpType::Symbol {
        if matches!(&*symbol_name(head), "for" | "while" | "repeat") {
            return true;
        }
        // A nested function's loops run when it is called, not when this body runs.
        if &*symbol_name(head) == "function" {
            return false;
        }
    }
    let mut cell = expression.cdr();
    while cell.kind() == SexpType::Pairlist {
        if contains_loop(cell.car(), depth - 1) {
            return true;
        }
        cell = cell.cdr();
    }
    false
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
        tail_updates: (Vec::new(), nil()),
        summary: nil(),
        bound: None,
        cold: Vec::new(),
    };
    let heap = crate::heap();
    let mut list = nil();
    for (tag, value) in values::pairlist_items(arguments).iter().rev() {
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
    /// `operator dst left right call function`
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
    /// `function expected target`: jumps unless the function is the primitive `expected`.
    BranchUnlessPrimitive,
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
    /// [`switch_table`]). The arms follow as a chain of [`Op::MatchLiteral`] tests.
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
}

/// Which replacement an [`Op::SubAssign`] does, stored as its last operand.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum ElementKind {
    /// `x[i] <- value`
    Single = 0,
    /// `x[[i]] <- value`
    Double = 1,
    /// `x$name <- value`
    Dollar = 2,
}

pub const OPS: [Op; 44] = [
    Op::Constant,
    Op::GetVar,
    Op::SetVar,
    Op::GetFun,
    Op::BranchUnlessBuiltin,
    Op::CallBuiltin,
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
    Op::BranchUnlessPrimitive,
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
}

pub const ARITHS: [(Arith, &str); 11] = [
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
];

/// Type feedback: the kinds of value an instruction produced, one bit per kind. A scalar is a
/// length-one vector and a plain vector is any other; both have no attributes and are not
/// ALTREP. An Array of logical, integer or double items is its own kind at every length, because
/// it promises what a plain vector does not (no NA, no attributes) and its operators differ.
pub mod feedback {
    pub const INTEGER_SCALAR: u16 = 1 << 0;
    pub const DOUBLE_SCALAR: u16 = 1 << 1;
    pub const LOGICAL_SCALAR: u16 = 1 << 2;
    pub const INTEGER_VECTOR: u16 = 1 << 3;
    pub const DOUBLE_VECTOR: u16 = 1 << 4;
    pub const LOGICAL_VECTOR: u16 = 1 << 5;
    pub const LIST: u16 = 1 << 6;
    pub const OTHER: u16 = 1 << 7;
    pub const INTEGER_ARRAY: u16 = 1 << 8;
    pub const DOUBLE_ARRAY: u16 = 1 << 9;
    pub const LOGICAL_ARRAY: u16 = 1 << 10;
}

/// The [`feedback`] bit for `value`.
pub unsafe fn value_kind(value: Sexp) -> u16 {
    if value.is_altrep() {
        return feedback::OTHER;
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
    /// The loops the code being compiled is inside, innermost last.
    open_loops: Vec<usize>,
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
    /// The body's calls in tail position that are `f(p, ...)` for a library builtin that
    /// updates its first argument in place, `p` the first parameter, with `p`: each becomes
    /// [`Op::CallTailUpdate`] (see [`tail_updates`]).
    tail_updates: (Vec<Sexp>, Sexp),
    /// While a `match` guard compiles: its arm's bound names, which it reads from registers
    /// instead of the frame.
    bound: Option<Bound>,
    /// Guard fallbacks laid out after the body's `Return` (see [`Compiler::cold_fallback`]).
    cold: Vec<Cold>,
    /// The stored half of the effect-free fact, `NULL` for an argument's code.
    summary: Sexp,
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

/// Compiles a closure body, or reports that it is better left to the AST evaluator. The
/// compiler's vectors are not allocations the R code asked for, so `Rprofmem` does not see them.
unsafe fn compile(body: Sexp, function: Sexp) -> R<Option<Sexp>> {
    let threshold = crate::heap().swap_report_threshold(usize::MAX);
    let compiled = compile_unreported(body, function);
    crate::heap().swap_report_threshold(threshold);
    compiled
}

unsafe fn compile_unreported(body: Sexp, function: Sexp) -> R<Option<Sexp>> {
    let mut formal_symbols = FxHashSet::default();
    let mut formal = function.car();
    while formal.kind() == SexpType::Pairlist {
        formal_symbols.insert(formal.tag());
        formal = formal.cdr();
    }
    let mut compiler = Compiler {
        local_symbols: local_symbols(body, function.car()),
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
        bound: None,
        tail_updates: tail_updates(body, function),
        cold: Vec::new(),
        summary: nil(),
    };
    let slot = compiler.cache();
    compiler.summary =
        effects::summarize(function.car(), body, compiler.local_symbols.as_ref(), slot)?;
    compiler.assemble(body).map(Some)
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
/// Where it keeps the integer scalars 0, 1, 2, ..., one per arm, that [`Op::SwitchArm`] returns.
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
    let mut numbers = Vec::with_capacity(arms.len() + 1);
    for number in 0..=arms.len() {
        numbers.push(values::scalar_integer(number as i32)?);
    }
    let numbers = values::list_vector(&numbers)?;
    let table = values::list_vector(&[name_vector, name_arms, position_arms, numbers, call])?;
    Ok((table, arms))
}

/// The arm number a `switch` runs for `selector`, from the table [`switch_table`] built, with
/// the special's errors.
unsafe fn switch_arm(selector: Sexp, table: Sexp) -> R<Sexp> {
    let numbers = table.element(SWITCH_NUMBERS);
    let position_arms = values::ints(table.element(SWITCH_POSITION_ARMS));
    let arm = match eval::switch_selector(selector, table.element(SWITCH_CALL))? {
        SwitchSelector::Name(text) => {
            let names = table.element(SWITCH_NAMES);
            let matched = (0..length(names)).find(|&index| {
                let name = names.element(index);
                name == text || name.bytes() == text.bytes()
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
    Ok(numbers.element(arm as usize))
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
        if rebinds_first_argument(expression) {
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

/// Adds the symbols `expression` assigns locally; `false` when it reflects on its frame.
unsafe fn collect_bindings(expression: Sexp, found: &mut FxHashSet<Sexp>) -> bool {
    if expression.kind() != SexpType::Language {
        return true;
    }
    let head = expression.car();
    if head.kind() == SexpType::Symbol {
        let name = symbol_name(head);
        if matches!(
            &*name,
            "assign"
                | "eval"
                | "evalq"
                | "delayedAssign"
                | "makeActiveBinding"
                | "local"
                | "sys.function"
                | "environment"
                | "list2env"
                | "sys.frame"
                | "sys.frames"
                | "attach"
                | "with"
                | "within"
                | "source"
                | "rm"
                | "remove"
                | "lockBinding"
                | "on.exit"
                | "Recall"
                | "do.call"
        ) {
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
    }
    let mut cell = expression;
    while cell.kind() == SexpType::Pairlist || cell.kind() == SexpType::Language {
        if !collect_bindings(cell.car(), found) {
            return false;
        }
        cell = cell.cdr();
    }
    true
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
        heap.set_element(constants, PROFILE, profile);
        heap.set_element(constants, TIER1, nil());
        heap.set_element(constants, SUMMARY, compiler.summary);
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
        let caches = values::alloc(SexpType::List, compiler.caches as usize * 2)?;
        heap.mark_weak_pairs(caches);
        let code = values::triple(SexpType::Bytecode, words, constants, caches)?;
        Ok(code)
    }

    /// The constant for a call that may call a closure, followed by the arguments its promises
    /// are made from: the unbound marker when an argument is worth compiling, which
    /// [`Op::CallGeneric`] replaces with the compiled arguments the first time it calls a
    /// closure here, or `NULL` when none is. A third constant is `TRUE` when a call of an
    /// effect-free closure may bind symbol arguments to their values (see [`eager_site`]).
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
        self.code.push(op_word(op));
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

    /// Compiles what runs when one of `guards` fails: the general evaluation of `call` into
    /// `dst`, after which execution continues here. Outside loops the fallback goes after the
    /// body's `Return`, so the inline path runs straight on instead of jumping over it; inside a
    /// loop it stays in place, where the loop's range covers it for `break` and `next`.
    unsafe fn cold_fallback(&mut self, guards: &[usize], call: Sexp, dst: u32) -> R<()> {
        if !self.open_loops.is_empty() {
            let done = self.emit_patchable(Op::Jump, &[0]);
            for &guard in guards {
                self.patch(guard);
            }
            self.fallback(call, dst)?;
            self.patch(done);
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
                ("<-" | "=", 2) => plain && args[0].1.kind() == SexpType::Symbol,
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
    /// replacement function, and the index expression (`"name"` for `x$name`), when `x` is a
    /// variable and `i` is a constant or a variable that is never a promise (so evaluating it
    /// early is unobservable).
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
            .all(|(_, index)| self.side_effect_free_index(*index, 0))
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
        if matches!(&*getter, "[" | "[[" | "$" | "@") {
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
                    && (constant(*value) || self.side_effect_free_index(*value, 0))
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
        let probe = self.frame_may_bind(head) as u32;
        let symbol = self.constant(head);
        let expected = self.constant(primitive);
        let cache = self.cache();
        let guard = self.emit_patchable(Op::Guard, &[symbol, expected, cache, probe, 0]);
        let rest = values::pairlist_items(target.cdr().cdr());
        let count = rest.len() as u32 + 2;
        let mark = self.next_register;
        let base = self.next_register;
        for _ in 0..count {
            self.allocate();
        }
        self.expression(args[1].1, base + count - 1)?;
        for (index, (_, value)) in rest.iter().enumerate() {
            self.expression(*value, base + 1 + index as u32)?;
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
        self.cold_fallback(&[guard], call, dst)?;
        Ok(())
    }

    /// Whether evaluating the subscript `index` cannot change a variable of the frame, so the
    /// compiled assignment may evaluate it before reading the target: a variable of the frame (a
    /// formal's promise runs in the caller's frame), a numeric constant, or base arithmetic on
    /// those.
    unsafe fn side_effect_free_index(&self, index: Sexp, depth: u32) -> bool {
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
            SexpType::Language if depth < 8 && index.car().kind() == SexpType::Symbol => {
                let operator = symbol_name(index.car());
                let arguments = values::pairlist_items(index.cdr());
                ARITHS[..5].iter().any(|(_, name)| *name == &*operator)
                    && (1..=2).contains(&arguments.len())
                    && !self.frame_may_bind(index.car())
                    && arguments.iter().all(|(tag, argument)| {
                        *tag == nil() && self.side_effect_free_index(*argument, depth + 1)
                    })
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
            let probe = self.frame_may_bind(symbol) as u32;
            let symbol = self.constant(symbol);
            let expected = self.constant(expected);
            let cache = self.cache();
            guards.push(self.emit_patchable(Op::Guard, &[symbol, expected, cache, probe, 0]));
        }
        let mark = self.next_register;
        let value = self.allocate();
        self.expression(args[1].1, value)?;
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
            [index, ..] => self.emit(
                Op::SubAssign,
                &[dst, variable, index, value, call_constant, kind as u32],
            ),
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
        let symbol = self.constant(head);
        let expected = self.constant(function);
        let cache = self.cache();
        let probe = self.frame_may_bind(head) as u32;
        let guard = self.emit_patchable(Op::Guard, &[symbol, expected, cache, probe, 0]);
        let call_constant = self.constant(call);
        let mark = self.next_register;
        let function_register = self.allocate();
        self.emit(Op::Constant, &[function_register, expected]);
        if self.tail_updates.0.contains(&call) {
            let target = self.constant(self.tail_updates.1);
            self.emit(
                Op::CallTailUpdate,
                &[dst, function_register, call_constant, target],
            );
            self.release_to(mark);
            self.cold_fallback(&[guard], call, dst)?;
            return Ok(());
        }
        let base = self.next_register;
        for _ in args {
            self.allocate();
        }
        for (index, (_, value)) in args.iter().enumerate() {
            self.expression(*value, base + index as u32)?;
        }
        self.emit(
            Op::CallBuiltin,
            &[
                dst,
                function_register,
                call_constant,
                base,
                args.len() as u32,
            ],
        );
        self.release_to(mark);
        self.cold_fallback(&[guard], call, dst)?;
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
        let symbol = self.constant(head);
        let expected = self.constant(primitive);
        let cache = self.cache();
        let probe = self.frame_may_bind(head) as u32;
        let guard = self.emit_patchable(Op::Guard, &[symbol, expected, cache, probe, 0]);
        self.inline(call, primitive, dst, name, args)?;
        self.cold_fallback(&[guard], call, dst)?;
        Ok(())
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
                let numbers = table.element(SWITCH_NUMBERS);
                let table = self.constant(table);
                self.emit(Op::SwitchArm, &[dst, dst, table]);
                let mut done = Vec::with_capacity(arms.len());
                for (index, body) in arms.iter().enumerate() {
                    let number = self.constant(numbers.element(index + 1));
                    let skip = self.emit_patchable(Op::MatchLiteral, &[dst, number, 0]);
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
                self.emit(Op::Jump, &[top]);
                self.patch(exit);
                self.close_loop(loop_index);
                self.loop_value(dst);
            }
            "repeat" => {
                let loop_index = self.open_loop();
                let top = self.here();
                self.loops[loop_index].step = top;
                self.expression(args[0].1, dst)?;
                self.emit(Op::Jump, &[top]);
                self.close_loop(loop_index);
                self.loop_value(dst);
            }
            "break" | "next" => {
                let loop_index = *self
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
            "<-" | "=" if reuses_first_argument(call) => self.fallback(call, dst)?,
            "<-" | "=" => {
                if rebinds_first_argument(call) {
                    self.rebinding = Some((args[1].1, args[0].1));
                }
                let compiled = self.expression(args[1].1, dst);
                self.rebinding = None;
                compiled?;
                let symbol = self.constant(args[0].1);
                let slot = self.cache();
                self.emit(Op::SetVar, &[symbol, dst, slot]);
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
                let formals = self.constant(nil());
                let body = self.constant(expression);
                let srcref = self.constant(nil());
                self.emit(Op::Closure, &[function, formals, body, srcref]);
                let call_constant = self.call_site(immediate_call)?;
                self.emit(Op::CallGeneric, &[dst, function, call_constant]);
                self.release_to(mark);
            }
            "function" => {
                let formals = self.constant(args[0].1);
                let body = self.constant(args[1].1);
                let srcref = self.constant(args.get(2).map_or(nil(), |(_, srcref)| *srcref));
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
                self.expression(args[1].1, right)?;
                let call_constant = self.constant(call);
                let function = self.constant(primitive);
                self.emit(
                    Op::Arithmetic,
                    &[arith as u32, dst, left, right, call_constant, function],
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
        self.emit(Op::Jump, &[step]);
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
        self.open_loops.push(self.loops.len() - 1);
        self.loops.len() - 1
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
            let op = OPS[(self.code[position] & OP_MASK) as usize];
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

    /// Whether a call of `primitive` with `args` can evaluate its subject first and, when the
    /// subject is not an object, its other arguments as values, which is what a dispatch-or-eval
    /// special such as `[` does. A formal among the other arguments may be missing, which the
    /// special passes on as an empty argument but a register cannot hold, so it takes the
    /// generic path.
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
                    || (!self.formals.contains(&value) && eval::dot_dot_index(value).is_none()))
        };
        Ok(*subject != missing_arg()
            && *subject != symbols().dots
            && rest.iter().all(|(_, value)| plain_argument(*value)))
    }

    /// A dispatch-or-eval special call: the subject evaluates first, an object dispatches with
    /// the other arguments as promises, and any other subject calls the special on values, the
    /// path the optimizing tier compiles.
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
        let generic = self.emit_patchable(Op::BranchUnlessPrimitive, &[function, expected, 0]);
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
        let symbol = self.constant(head);
        let expected = self.constant(primitive);
        let cache = self.cache();
        let probe = self.frame_may_bind(head) as u32;
        let guard = self.emit_patchable(Op::Guard, &[symbol, expected, cache, probe, 0]);
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
        self.cold_fallback(&[guard], call, dst)?;
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
        let symbol = self.constant(head);
        let expected = self.constant(primitive);
        let cache = self.cache();
        let probe = self.frame_may_bind(head) as u32;
        let guard = self.emit_patchable(Op::Guard, &[symbol, expected, cache, probe, 0]);
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
        self.cold_fallback(&[guard], call, dst)?;
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
        let generic = self.emit_patchable(Op::BranchUnlessPrimitive, &[function, expected, 0]);
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
        self.emit(Op::GetFun, &[function, symbol, cache, call_constant]);
        let args = values::pairlist_items(call.cdr());
        // A register holds an argument's value without counting as a reference, so a later
        // argument that modifies a variable in place would change an earlier argument's value.
        let simple = args.iter().all(|(_, value)| *value != symbols().dots)
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
            self.emit(
                Op::CallBuiltin,
                &[dst, function, call_constant, base, args.len() as u32],
            );
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
        Op::JumpUnless => 3,
        Op::Arithmetic => 6,
        Op::Return => 1,
        Op::ForInit => 3,
        Op::ForStep => 4,
        Op::ForEnd => 1,
        Op::Invisible | Op::Visible => 0,
        Op::LogicLeft => 5,
        Op::LogicRight => 5,
        Op::Closure => 4,
        Op::SubAssign => 6,
        Op::SubAssignMatrix => 6,
        Op::Replace => 8,
        Op::SetSrcref => 2,
        Op::EndStatement => 3,
        Op::BranchUnlessPrimitive => 3,
        Op::BranchIfObject => 2,
        Op::DispatchSubject => 4,
    }
}

/// [`operand_count`] by opcode word, so dispatch reads the width with one load beside the
/// opcode instead of branching on the decoded op.
/// An instruction's first code word: the opcode in the low byte and the operand count above
/// it, so the interpreter decodes both without a table load.
pub const fn op_word(op: Op) -> u32 {
    op as u32 | (operand_count(op) as u32) << OP_BITS
}

/// The opcode of the code word `word`, unless it is not a valid one.
pub fn op_of(word: u32) -> Option<Op> {
    OPS.get((word & OP_MASK) as usize)
        .copied()
        .filter(|&op| op_word(op) == word)
}

const OP_BITS: u32 = 8;
const OP_MASK: u32 = (1 << OP_BITS) - 1;

// The interpreter turns a code word's low byte back into an `Op` by its discriminant.
const _: () = {
    let mut index = 0;
    while index < OPS.len() {
        assert!(OPS[index] as usize == index && operand_count(OPS[index]) < 1 << 24);
        index += 1;
    }
    assert!(OPS.len() <= OP_MASK as usize);
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
        Op::ForInit => &[Loop, Register, Constant],
        Op::ForStep => &[Loop, Register, Symbol, Target],
        Op::Invisible | Op::Visible => &[],
        Op::LogicLeft => &[Register, Register, Constant, Immediate, Target],
        Op::LogicRight => &[Register, Register, Register, Constant, Immediate],
        Op::Closure => &[Register, Formals, Constant, Constant],
        Op::SubAssign => &[Register, Symbol, Register, Register, Call, Immediate],
        Op::SubAssignMatrix => &[Register, Symbol, Register, Register, Register, Call],
        Op::Replace => &[Register, Symbol, Symbol, Cache, Call, Base, CountAtLeastOne, Call],
        Op::SetSrcref => &[Constant, Immediate],
        Op::EndStatement => &[Register, Base, Base],
        Op::BranchUnlessPrimitive => &[Register, Constant, Target],
        Op::DispatchSubject => &[Register, Register, Call, Register],
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
    let mut starts = vec![0u64; words.len().div_ceil(64)];
    let mut pc = 0;
    while pc < words.len() {
        let op = op_of(words[pc] as u32).ok_or("an invalid instruction")?;
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
    let is_start = |target: usize| {
        target < words.len() && starts[target / 64] & (1 << (target % 64)) != 0
    };
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
                    constant(value)?.kind() == SexpType::Language
                        && (arguments == unbound()
                            || matches!(arguments.kind(), SexpType::Nil | SexpType::Pairlist))
                        && constant(value + 2).is_ok()
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
                Switch => valid_switch_table(constant(value)?),
                Cache => value < caches,
                Target => is_start(value),
                Loop => value < loops,
                Operator => value < ARITHS.len(),
                Immediate => true,
            };
            if !valid {
                return Err(format!("an invalid operand {index} of {op:?} at {pc}"));
            }
        }
        if op == Op::EndStatement && operand(1) > operand(2) {
            return Err(format!("an invalid register range at {pc}"));
        }
        pc += 1 + operand_count(op);
    }
    Ok(())
}

/// Whether `table` has the shape [`switch_table`] gives it, which [`switch_arm`] reads unchecked.
unsafe fn valid_switch_table(table: Sexp) -> bool {
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
        || numbers.kind() != SexpType::List
        || table.element(SWITCH_CALL).kind() != SexpType::Language
    {
        return false;
    }
    let arms = length(numbers) as i64;
    // A name's arm indexes the numbers directly, while a negative position arm is an error
    // `switch_arm` raises before indexing.
    values::ints(name_arms).iter().all(|&arm| (0..arms).contains(&i64::from(arm)))
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
    /// The next cell, for a pairlist sequence.
    cell: Option<Sexp>,
}

impl LoopState {
    /// The state of a loop over a vector, which is all the optimizing tier compiles.
    pub fn vector(index: usize, count: usize) -> LoopState {
        LoopState {
            index,
            count,
            cell: None,
        }
    }
}

/// Stores the next element of an atomic `sequence` into the loop variable's current scalar
/// in place, as GNU R's `STEPFOR` does, instead of allocating a fresh scalar per iteration.
/// Only a scalar that nothing but the binding references may change: a count above one means
/// the body stored the value somewhere, and that copy must keep the old element.
unsafe fn overwrite_loop_variable(sequence: Sexp, index: usize, variable: Sexp, env: Sexp) -> bool {
    let kind = sequence.kind();
    if !matches!(kind, SexpType::Integer | SexpType::Real | SexpType::Logical)
        || sequence.is_altrep()
    {
        return false;
    }
    let Some(cell) = crate::heap().frame_cell(env, variable) else {
        return false;
    };
    if cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED) {
        return false;
    }
    let value = cell.cdr();
    if value.kind() != kind
        || value.is_altrep()
        || value.refcount() != 1
        || value.length() != 1
        || values::has_attributes(value)
    {
        return false;
    }
    match kind {
        SexpType::Real => value.slice_mut::<f64>()[0] = sequence.slice::<f64>()[index],
        _ => value.slice_mut::<i32>()[0] = sequence.slice::<i32>()[index],
    }
    true
}

/// The number of `for` loops in `code`.
pub unsafe fn loop_count(code: Sexp) -> usize {
    length(code.cdr().element(LOOPS)) / 4
}

/// Runs a compiled promise's code in `env`, the frame it was compiled for. Promise code has no
/// loops (`compiles_as_promise` rejects them) and runs for no closure, so this skips the entry
/// profile, the loop states and the optimizing tier that `run` sets up for a function body.
pub unsafe fn run_promise(code: Sexp, env: Sexp) -> R<Sexp> {
    let heap = crate::heap();
    heap.root(code);
    let mark = heap.handle_mark();
    let register_count = values::ints(code.cdr().element(REGISTERS))[0] as usize;
    let base = interp().bytecode.registers.len();
    reserve_registers(base + register_count)?;
    let was_compiled_frame = env.has_gp(gp::COMPILED_FRAME);
    env.set_gp_bits(gp::COMPILED_FRAME, true);
    let srcref = interp().srcref;
    let mut machine = Machine {
        function: nil(),
        code,
        env,
        base,
        mark,
        loops: &mut [],
    };
    let result = execute_code(&mut machine, 0);
    interp().srcref = srcref;
    interp().bytecode.registers.truncate(base);
    env.set_gp_bits(gp::COMPILED_FRAME, was_compiled_frame);
    match result {
        Err(Unwind::Error(failure)) => Err(conditions::signal_error(failure)),
        other => other,
    }
}

unsafe fn run(function: Sexp, code: Sexp, env: Sexp) -> R<Sexp> {
    let heap = crate::heap();
    heap.root(code);
    let mark = heap.handle_mark();
    let constants = code.cdr();
    let register_count = values::ints(constants.element(REGISTERS))[0] as usize;
    let base = interp().bytecode.registers.len();
    reserve_registers(base + register_count)?;
    let profile = values::ints_mut(constants.element(PROFILE));
    let entries = profile[profile::ENTRIES].saturating_add(1);
    profile[profile::ENTRIES] = entries;
    let mut loops: SmallVec<[LoopState; 4]> = smallvec![LoopState::default(); loop_count(code)];
    let was_compiled_frame = env.has_gp(gp::COMPILED_FRAME);
    env.set_gp_bits(gp::COMPILED_FRAME, true);
    let mut machine = Machine {
        function,
        code,
        env,
        base,
        mark,
        loops: &mut loops,
    };
    // Most bodies never compile, and asking the tier about every call of every body cost more
    // than the compiled code saved on a test suite.
    let entered = match interp().bytecode.tier {
        Some(tier)
            if constants.element(TIER1) != nil()
                || ((entries == 1 || entries >= tier.entry_threshold)
                    && profile[profile::TIER_STATE] >= 0) =>
        {
            (tier.enter)(&mut machine)
        }
        _ => None,
    };
    let result = match entered {
        Some(TierExit::Finished(result)) => result,
        Some(TierExit::Resume { pc, pending }) => match pending {
            Some(unwind) => continue_after(&mut machine, pc, unwind)
                .and_then(|pc| execute_code(&mut machine, pc)),
            None => execute_code(&mut machine, pc),
        },
        None => execute_code(&mut machine, 0),
    };
    interp().bytecode.registers.truncate(base);
    env.set_gp_bits(gp::COMPILED_FRAME, was_compiled_frame);
    match result {
        Err(Unwind::Error(failure)) => Err(conditions::signal_error(failure)),
        other => other,
    }
}

unsafe fn execute_code(machine: &mut Machine<'_>, pc: usize) -> R<Sexp> {
    let _activity = activity::enter(Activity::Interpreter, 0);
    interpret::<false>(machine, pc)
}

/// Where interpretation continues after the instruction at `pc` unwound: the loop target of a
/// `break` or `next` aimed at this activation, or nowhere, when the unwind propagates.
unsafe fn continue_after(machine: &mut Machine<'_>, pc: usize, unwind: Unwind) -> R<usize> {
    let Unwind::Jump(jump) = unwind else {
        return Err(unwind);
    };
    let loops = machine.code.cdr().element(LOOPS);
    let target = match *jump {
        Jump::Break { env: target } if target == machine.env => loop_target(loops, pc, true),
        Jump::Next { env: target } if target == machine.env => loop_target(loops, pc, false),
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
/// does not specialize. It must not be a jump, a branch, `Return` or a loop instruction.
pub unsafe fn step_one(machine: &mut Machine<'_>, pc: usize) -> R<()> {
    interpret::<true>(machine, pc).map(|_| ())
}

/// What `GetVar` at `pc` reads, and whether the binding is in the activation's own frame.
pub unsafe fn load_variable(code: Sexp, env: Sexp, pc: usize) -> R<(Sexp, bool)> {
    let words = values::ints(code.car());
    let constants = code.cdr();
    let symbol = constants.element(words[pc + 2] as usize);
    let local = crate::heap().frame_cell(env, symbol).is_some();
    let value = lookup_variable(
        symbol,
        env,
        LookupCache::of(code),
        words[pc + 3] as usize,
        0..0,
    )?;
    record_feedback(constants, pc, value);
    Ok((value, local))
}

/// What every instruction of an activation reads besides its code and registers, loaded once
/// instead of once per instruction. None of these addresses changes while the activation runs:
/// the code object is rooted and never patched, its vectors are never resized, and the register
/// window was reserved on entry in a file that never reallocates. What they point at (registers,
/// cache slots) does change.
struct Activation<'m, 'a> {
    machine: &'m mut Machine<'a>,
    constants: Sexp,
    cache: LookupCache,
    /// The type feedback words, one per code word, or null while nothing will read them: no
    /// optimizing tier is installed, or the code is not yet warm (see [`warm`]).
    feedback: *mut i32,
    env: Sexp,
    mark: usize,
    heap: *mut Heap,
    interp: *mut Interp,
    code: *const i32,
    registers: *mut Sexp,
    /// What the activation returns once a handler stops the chain by answering [`STOP`].
    exit: R<Sexp>,
}

impl Activation<'_, '_> {
    #[inline(always)]
    unsafe fn constant(&self, instruction: Instruction, index: usize) -> Sexp {
        self.constants.element(instruction.operand(index))
    }

    #[inline(always)]
    unsafe fn record(&self, pc: usize, value: Sexp) {
        if !self.feedback.is_null() {
            *self.feedback.add(pc) |= i32::from(value_kind(value));
        }
    }
}

/// Whether tier 0 records type feedback for this code: always for a body with a loop, and for
/// any other once it has optimized code or is halfway to the entry threshold. Recording costs a
/// type test and a store per value, and most bodies never get near a threshold, so recording
/// from the first entry made every call of cold package code slower with the optimizing tier
/// installed than without it. Half a threshold of activations still sees every type a compile
/// would specialize on. A loop is different: it can turn hot inside a body's only activation,
/// and compiling it needs the types of the instructions that ran before it, once.
unsafe fn warm(constants: Sexp, tier: &Tier) -> bool {
    if constants.element(TIER1) != nil() || length(constants.element(LOOPS)) > 0 {
        return true;
    }
    let profile = values::ints(constants.element(PROFILE));
    profile[profile::TIER_STATE] >= 0 && profile[profile::ENTRIES] >= tier.entry_threshold / 2
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
unsafe fn interpret<const SINGLE: bool>(machine: &mut Machine<'_>, pc: usize) -> R<Sexp> {
    let constants = machine.code.cdr();
    let feedback = match interp().bytecode.tier {
        Some(tier) if warm(constants, &tier) => {
            values::ints_mut(constants.element(FEEDBACK)).as_mut_ptr()
        }
        _ => std::ptr::null_mut(),
    };
    let code = values::ints(machine.code.car()).as_ptr();
    let registers = register_address(machine.base);
    let mut state = Activation {
        constants,
        cache: LookupCache::of(machine.code),
        feedback,
        env: machine.env,
        mark: machine.mark,
        heap: crate::heap(),
        interp: interp(),
        code,
        registers,
        exit: Ok(nil()),
        machine,
    };
    let handlers = if SINGLE {
        &STEP_HANDLERS
    } else {
        &RUN_HANDLERS
    };
    if !SINGLE {
        release(&state);
    }
    (*handlers.as_ptr().add(op_index(code, pc)))(&mut state, pc);
    state.exit
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
        $(
            unsafe fn $name<const SINGLE: bool>(state: &mut Activation<'_, '_>, at: usize) {
                let instruction = Instruction { code: state.code, at, registers: state.registers };
                let flow = $body(state, instruction);
                let pc = advance::<SINGLE>(state, at, at + 1 + operand_count(Op::$op), flow);
                if SINGLE || pc == STOP {
                    return;
                }
                if $roots {
                    release(state);
                }
                become (*RUN_HANDLERS.as_ptr().add(op_index(state.code, pc)))(state, pc)
            }
        )*

        const fn handler_table<const SINGLE: bool>() -> [Handler; OPS.len()] {
            let mut table: [Handler; OPS.len()] = [jump::<SINGLE>; OPS.len()];
            $(table[Op::$op as usize] = $name::<SINGLE>;)*
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
        Ok(Flow::Done(value)) => {
            state.exit = Ok(value);
            STOP
        }
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
        Err(unwind) => {
            state.exit = Err(unwind);
            STOP
        }
    }
}

static RUN_HANDLERS: [Handler; OPS.len()] = handler_table::<false>();
static STEP_HANDLERS: [Handler; OPS.len()] = handler_table::<true>();

handlers! {
    run_constant: Constant, false, op_constant;
    run_get_var: GetVar, true, op_get_var;
    run_set_var: SetVar, true, op_set_var;
    run_get_fun: GetFun, true, op_get_fun;
    run_branch_unless_builtin: BranchUnlessBuiltin, false, op_branch_unless_builtin;
    run_call_builtin: CallBuiltin, true, op_call_builtin;
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
    run_branch_unless_primitive: BranchUnlessPrimitive, false, op_branch_unless_primitive;
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
    run_sub_assign: SubAssign, true, op_sub_assign;
    run_sub_assign_matrix: SubAssignMatrix, true, op_sub_assign_matrix;
    run_replace: Replace, true, op_replace;
}

/// `Jump`, the one instruction whose failure resumes somewhere other than after itself: the
/// optimizing tier can take over at a back edge and hand back an unwind from a later position.
/// It also fills the table slots of the ops listed in `handlers!` before they are overwritten.
unsafe fn jump<const SINGLE: bool>(state: &mut Activation<'_, '_>, at: usize) {
    let target = *state.code.add(at + 1) as usize;
    let mut pc = target;
    if target < at {
        if let Err(unwind) = crate::unwind::poll_interrupt() {
            pc = failed::<SINGLE>(state, at, unwind);
        } else if let Some(tier) = (*state.interp).bytecode.tier {
            match back_edge(state.machine, &tier, pc) {
                BackEdge::Continue(resume) => pc = resume,
                BackEdge::Finished(result) => {
                    state.exit = result;
                    return;
                }
                BackEdge::Unwound { pc: resume, unwind } => {
                    pc = failed::<SINGLE>(state, resume, unwind);
                }
            }
        }
    }
    if SINGLE || pc == STOP {
        return;
    }
    release(state);
    become (*RUN_HANDLERS.as_ptr().add(op_index(state.code, pc)))(state, pc)
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
    let value = lookup_variable(symbol, state.env, state.cache, instruction.operand(2), held)?;
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_set_var(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let symbol = state.constant(instruction, 0);
    let value = instruction.input(1);
    let hint = &mut *state.cache.hints.add(instruction.operand(2));
    let cell = (*state.heap).frame_cell_hinted(state.env, symbol, hint);
    eval::assign_local(symbol, value, state.env, cell)?;
    (*state.interp).visible = false;
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_get_fun(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let symbol = state.constant(instruction, 1);
    let call = state.constant(instruction, 3);
    let function = lookup_function(symbol, state.env, state.cache, instruction.operand(2), call)?;
    instruction.output(0, function);
    Ok(Flow::Next)
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
    let definition = eval::builtin_def(function)?;
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
    let value = call_builtin(definition, call, function, &args, state.env)?;
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_call_generic(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let function = instruction.input(1);
    let call = state.constant(instruction, 2);
    let value = if function.kind() == SexpType::Closure {
        let mut compiled = state.constants.element(instruction.operand(2) + 1);
        if compiled == unbound() {
            compiled = compile_promise_arguments(state.machine.function, call.cdr())?;
            (*state.heap).set_element(state.constants, instruction.operand(2) + 1, compiled);
        }
        let arguments = if compiled == nil() {
            call.cdr()
        } else {
            compiled
        };
        let mut args = eval::Args::new();
        // Binding a variable's value instead of a promise for it is invisible when the callee
        // cannot assign anything before forcing it, nor look at the promise (`substitute`,
        // `missing`), and no handler can run R code in between.
        let eager = state.constants.element(instruction.operand(2) + 2);
        if eager != nil()
            && !effects::calling_handler_established()
            && effects::effect_free(function)?
        {
            eval::eager_args_into(arguments, state.env, &mut args)?;
        } else {
            eval::compiled_args_into(arguments, state.env, &mut args)?;
        }
        eval::apply_closure_owning(call, function, &args, state.env, None)?
    } else {
        // A special reads its arguments as expressions, so they stay uncompiled.
        eval::apply_call(call, function, call.cdr(), state.env)?
    };
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_call_rebinding(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let function = instruction.input(1);
    let call = state.constant(instruction, 2);
    let target = state.constant(instruction, 3);
    let value = eval::rebind_call(call, function, target, state.env)?;
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_call_tail_update(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let function = instruction.input(1);
    let call = state.constant(instruction, 2);
    let target = state.constant(instruction, 3);
    let value = eval::tail_update(call, function, target, state.env)?;
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
        None => crate::subset::dollar_value(object, name.car(), call, state.env)?,
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
    let value = switch_arm(instruction.input(1), state.constant(instruction, 2))?;
    instruction.output(0, value);
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
    let value = crate::records::eval_bound_registers(expression, names, bound, state.env)?;
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
unsafe fn op_branch_unless_primitive(
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
    let mut args = eval::Args::new();
    let value = match dispatch::dispatch_evaluated_subject(
        definition,
        call,
        function,
        call.cdr(),
        instruction.input(3),
        state.env,
        &mut args,
    )? {
        Some(value) => value,
        None => call_builtin(definition, call, function, &args, state.env)?,
    };
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_ast(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let expression = state.constant(instruction, 1);
    let value = eval::eval(expression, state.env)?;
    state.record(instruction.at, value);
    instruction.output(0, value);
    Ok(Flow::Next)
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
    let enclosure = state.env.enclosure();
    if instruction.operand(3) == 0
        && !enclosure.has_gp(gp::FUNCTION_FRAME)
        && let Some(cell) = cached_cell(symbol, enclosure, state.cache, slot)
        && cell.cdr() == expected
    {
        return Ok(Flow::Next);
    }
    let found = if instruction.operand(3) == 0 {
        lookup_enclosing_function(symbol, state.env, state.cache, slot)
    } else {
        lookup_function(symbol, state.env, state.cache, slot, unbound())
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
        return Ok(Flow::Goto(instruction.operand(2)));
    }
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_arithmetic(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let left = instruction.input(2);
    let right = instruction.input(3);
    let value = match fast_arithmetic(ARITHS[instruction.operand(0)].0, left, right)? {
        Some(value) => value,
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
            call_builtin(definition, call, function, &args, state.env)?
        }
    };
    state.record(instruction.at, value);
    instruction.output(1, value);
    (*state.interp).visible = true;
    Ok(Flow::Next)
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
    let sequence = values::materialize(sequence)?;
    let count = match sequence.kind() {
        SexpType::Nil => 0,
        SexpType::Pairlist | SexpType::Language => values::pairlist_length(sequence),
        kind if kind.is_vector() => sequence.length() as usize,
        _ => return Err(error_in(call, "invalid for() loop sequence")),
    };
    // The body must see the sequence as shared, so that modifying the variable it came from
    // copies it instead of changing the elements still to come.
    sequence.increment_refcount();
    state.record(instruction.at, sequence);
    instruction.output(1, sequence);
    // Promise code runs without loop states, so only code read from a file could name one here.
    *state
        .machine
        .loops
        .get_mut(instruction.operand(0))
        .ok_or_else(|| error("compiled code names a loop it does not have"))? = LoopState {
        index: 0,
        count,
        cell: Some(sequence),
    };
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_for_step(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let env = state.env;
    let variable = state.constant(instruction, 2);
    let loop_state = state
        .machine
        .loops
        .get_mut(instruction.operand(0))
        .ok_or_else(|| error("compiled code names a loop it does not have"))?;
    if loop_state.index >= loop_state.count {
        return Ok(Flow::Goto(instruction.operand(3)));
    }
    let sequence = instruction.input(1);
    // The compiler steps the register its `ForInit` filled, which keeps the index in bounds;
    // code read from a file is only verified operand by operand.
    if sequence.kind().is_vector() && loop_state.index >= sequence.length() as usize {
        return Err(error("compiled code steps a loop over another sequence"));
    }
    let index = loop_state.index;
    loop_state.index += 1;
    if !overwrite_loop_variable(sequence, index, variable, env) {
        let element = match sequence.kind() {
            SexpType::List | SexpType::Expression => sequence.element(index),
            SexpType::Pairlist | SexpType::Language => {
                let cell = loop_state.cell.unwrap_or(sequence);
                loop_state.cell = Some(cell.cdr());
                cell.car()
            }
            _ => crate::subset::vector_element(sequence, index)?,
        };
        eval::define(variable, element, env)?;
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
    let closure = eval::make_closure(formals, body, srcref, state.env)?;
    // Every closure this site creates shares one compiled body, as GNU R compiles inner
    // functions with their outer one. Compiling per closure object would leave a function
    // created anew on each call (a `vapply` callback, say) on the AST evaluator, or recompile it
    // every other call. The code depends on the formals and body only: its inline caches and the
    // optimizing tier both check the enclosure they were filled for.
    if body.kind() != SexpType::Bytecode && worth_compiling(body) {
        // A body the compiler rejects (an error is its way of saying so, such as for a `break`
        // outside a loop) stays source, which the AST evaluator runs.
        if let Ok(Some(code)) = compile(body, closure) {
            (*state.heap).set_element(state.constants, instruction.operand(2), code);
            (*state.heap).set_cdr(closure, code);
        }
    }
    instruction.output(0, closure);
    (*state.interp).visible = true;
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_sub_assign(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let env = state.env;
    let call = state.constant(instruction, 4);
    let value = instruction.input(3);
    let variable = state.constant(instruction, 1);
    let kind = match instruction.operand(5) {
        0 => ElementKind::Single,
        1 => ElementKind::Double,
        _ => ElementKind::Dollar,
    };
    let done = match subscript_value(env, instruction.input(2))? {
        Some(index) => assign_element_in_place(env, variable, index, value, kind)?,
        None => false,
    };
    if !done {
        eval::complex_assign(call, call.cdr().car(), value, env, false)?;
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
    let env = state.env;
    let call = state.constant(instruction, 5);
    let value = instruction.input(4);
    let variable = state.constant(instruction, 1);
    let done = match (
        subscript_value(env, instruction.input(2))?,
        subscript_value(env, instruction.input(3))?,
    ) {
        (Some(row), Some(column)) => {
            assign_matrix_element_in_place(env, variable, row, column, value)?
        }
        _ => false,
    };
    if !done {
        eval::complex_assign(call, call.cdr().car(), value, env, false)?;
    }
    instruction.output(0, value);
    (*state.interp).visible = false;
    Ok(Flow::Next)
}

#[inline(always)]
unsafe fn op_replace(state: &mut Activation<'_, '_>, instruction: Instruction) -> R<Flow> {
    let env = state.env;
    let whole = state.constant(instruction, 7);
    let (first, count) = (instruction.operand(5), instruction.operand(6));
    let value = instruction.register(first + count - 1);
    let done = replace_variable(
        env,
        state.constant(instruction, 1),
        (
            state.constant(instruction, 2),
            state.cache,
            instruction.operand(3),
        ),
        state.constant(instruction, 4),
        whole,
        std::slice::from_raw_parts_mut(instruction.registers.add(first), count),
    )?;
    if !done {
        eval::complex_assign(whole, whole.cdr().car(), value, env, false)?;
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

#[inline(never)]
unsafe fn back_edge(machine: &mut Machine<'_>, tier: &Tier, pc: usize) -> BackEdge {
    let profile = values::ints_mut(machine.code.cdr().element(PROFILE));
    profile[profile::BACK_EDGES] = profile[profile::BACK_EDGES].saturating_add(1);
    if profile[profile::BACK_EDGES] < tier.back_edge_threshold {
        return BackEdge::Continue(pc);
    }
    profile[profile::BACK_EDGES] = 0;
    match (tier.back_edge)(machine, pc) {
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
    let heap = crate::heap();
    let Some(cell) = heap.frame_cell(env, variable) else {
        return Ok(false);
    };
    let target = cell.cdr();
    if cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED)
        || target == value
        || target.is_object()
        || target.is_array()
        || target.is_altrep()
        || index.is_altrep()
        || values::has_attributes(index)
        || length(index) != 1
    {
        return Ok(false);
    }
    let fits = match target.kind() {
        SexpType::List => kind != ElementKind::Single && value != nil(),
        SexpType::Real | SexpType::Integer | SexpType::Logical | SexpType::String => {
            kind != ElementKind::Dollar
                && value.kind() == target.kind()
                && length(value) == 1
                && !value.is_altrep()
                && !values::has_attributes(value)
        }
        _ => false,
    };
    if !fits {
        return Ok(false);
    }
    let position = match index.kind() {
        // `$<-` on a list replaces the first element whose name matches exactly; a name `x`
        // lacks appends an element, which `complex_assign` does.
        SexpType::String => {
            let wanted = index.element(0);
            let names = values::get_attr(target, symbols().names);
            if wanted == values::na_string() || names.kind() != SexpType::String {
                return Ok(false);
            }
            let wanted_bytes = wanted.bytes();
            let found = (0..length(names)).find(|&at| {
                let name = names.element(at);
                name == wanted || (name != values::na_string() && name.bytes() == wanted_bytes)
            });
            match found {
                Some(at) => at as f64 + 1.0,
                None => return Ok(false),
            }
        }
        SexpType::Integer if index.slice::<i32>()[0] != NA_INTEGER => {
            f64::from(index.slice::<i32>()[0])
        }
        SexpType::Real => index.slice::<f64>()[0],
        _ => return Ok(false),
    };
    let count = length(target);
    // Assigning one past the end appends, which a vector without attributes that a previous
    // growth left room in (see `subset::grow`) does in place.
    let appends = position == count as f64 + 1.0
        && heap.is_resizable(target)
        && target.capacity() > count as isize
        && target.attrib() == nil();
    // NaN fails both comparisons, and `x[[2.7]]` truncates to the second element.
    if !appends && !(position >= 1.0 && position < count as f64 + 1.0) {
        return Ok(false);
    }
    let position = position as usize - 1;
    // The conditions under which `complex_assign` updates the variable's value in place.
    moves::modifying(target)?;
    if values::maybe_shared(target) || !eval::reuse_permitted(env, target) {
        return Ok(false);
    }
    if appends {
        heap.resize_vector(target, count as isize + 1)?;
    }
    match target.kind() {
        SexpType::Real => values::reals_mut(target)[position] = value.slice::<f64>()[0],
        SexpType::Integer | SexpType::Logical => {
            values::ints_mut(target)[position] = value.slice::<i32>()[0]
        }
        SexpType::String => heap.set_element(target, position, value.element(0)),
        _ => heap.set_element(target, position, value),
    }
    cell.set_gp_bits(gp::BINDING_MISSING | gp::BINDING_ABSENT, false);
    Ok(true)
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

/// `variable[row, column] <- value` on an atomic matrix, done in place when the matrix is the
/// frame's own unshared value, `value` is one element of its type and both subscripts are in
/// bounds. Returns false, having changed nothing, when the general assignment must run.
/// [`Op::Replace`]'s one-level update of a local variable, as `complex_assign` runs it, through
/// a builtin replacement or a closure that takes only the target and the value: `false`
/// leaves the assignment to `complex_assign`. `args` are the call's argument registers, the
/// first for the variable's value and the last holding the value assigned.
unsafe fn replace_variable(
    env: Sexp,
    variable: Sexp,
    (setter, cache, slot): (Sexp, LookupCache, usize),
    setter_call: Sexp,
    whole: Sexp,
    args: &mut [Sexp],
) -> R<bool> {
    let heap = crate::heap();
    let Some(cell) = heap.frame_cell(env, variable) else {
        return Ok(false);
    };
    if cell.has_gp(gp::BINDING_LOCKED | gp::ACTIVE_BINDING) {
        return Ok(false);
    }
    let mut target = cell.cdr();
    if target.kind() == SexpType::Promise {
        target = target.car();
    }
    if target == unbound() || target == missing_arg() {
        return Ok(false);
    }
    // `complex_assign` reads the variable before it looks the replacement up, so a missing
    // variable is the error both report.
    let function = lookup_function(setter, env, cache, slot, unbound())?;
    let builtin = match function.kind() {
        // An object may dispatch to a replacement method, which `complex_assign` calls with the
        // `*tmp*` and value promises it expects.
        SexpType::Builtin => {
            if target.is_object() {
                return Ok(false);
            }
            Some(eval::builtin_def(function)?)
        }
        SexpType::Closure if args.len() == 2 => None,
        _ => return Ok(false),
    };
    moves::modifying(target)?;
    // Registers count no reference, so an argument that is the target itself, as in
    // `names(x) <- x`, must not watch the replacement change it.
    if values::maybe_shared(target)
        || !eval::reuse_permitted(env, target)
        || args[1..].contains(&target)
    {
        let was_array = target.is_array();
        target = values::shallow_duplicate(target)?;
        target.set_array(was_array);
    }
    args[0] = target;
    heap.count_transient_binding(env, symbols().tmp);
    let state = interp();
    state.tmp_values.push((env, target));
    let result = match builtin {
        Some(definition) => {
            let mut arguments = eval::Args::new();
            let mut item = setter_call.cdr();
            for &value in args.iter() {
                arguments.push(Arg {
                    tag: item.tag(),
                    value,
                });
                item = item.cdr();
            }
            let outer = state
                .assignment_call
                .replace((setter_call, whole, state.frames.len()));
            let result = eval::call_builtin(definition, setter_call, function, &arguments, env);
            interp().assignment_call = outer;
            result
        }
        None => replace_with_closure(function, setter, whole, target, args[1], env),
    };
    interp().tmp_values.pop();
    eval::assign_local(variable, result?, env, Some(cell))?;
    Ok(true)
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
    let target = cell.cdr();
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

unsafe fn call_builtin(
    definition: &'static BuiltinDef,
    call: Sexp,
    function: Sexp,
    args: &[Arg],
    env: Sexp,
) -> R<Sexp> {
    if definition.generic != Generic::None
        && let Some(value) = dispatch::try_builtin_dispatch(definition, call, function, args, env)?
    {
        return Ok(value);
    }
    eval::call_builtin(definition, call, function, args, env)
}

unsafe fn record_feedback(constants: Sexp, pc: usize, value: Sexp) {
    let feedback = constants.element(FEEDBACK);
    values::ints_mut(feedback)[pc] |= i32::from(value_kind(value));
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
            _ => Ok(None),
        };
    }
    let (a, b) = (as_real(left), as_real(right));
    let real = |value: f64| -> R<Option<Sexp>> { Ok(Some(values::scalar_real(value)?)) };
    let nan = a.is_nan() || b.is_nan();
    match operator {
        Arith::Add => real(a + b),
        Arith::Subtract => real(a - b),
        Arith::Multiply => real(a * b),
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
    let enclosure = match outside_function_frames(symbol, env.enclosure(), hint) {
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
                return holding(held, || eval::eval_symbol(symbol, env));
            }
            return Ok(value);
        }
    };
    if let Some(cell) = cached_cell(symbol, enclosure, cache, slot)
        && !cell.has_gp(gp::ACTIVE_BINDING)
    {
        let value = cell.cdr();
        if value.kind() == SexpType::Promise {
            return holding(held, || eval::eval_symbol(symbol, env));
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
    holding(held, || eval::eval_symbol(symbol, env))
}

/// Runs `run`, which forces a promise or an active binding and so runs code. The registers
/// below the destination hold values of the expression being evaluated without counting as
/// references, so they count as referenced while that code runs: in `x + y` where forcing `y`
/// runs `x[] <- 1`, the assignment must copy `x` rather than change the left operand.
unsafe fn holding(held: Range<usize>, run: impl FnOnce() -> R<Sexp>) -> R<Sexp> {
    let values: SmallVec<[Sexp; 8]> = interp()
        .bytecode
        .registers
        .get(held)
        .map(SmallVec::from_slice)
        .unwrap_or_default();
    for value in &values {
        value.increment_refcount();
    }
    let result = run();
    for value in &values {
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
    if let Some(cell) = cached_cell(symbol, enclosure, cache, slot)
        && !cell.has_gp(gp::ACTIVE_BINDING)
    {
        let mut value = cell.cdr();
        if value.kind() == SexpType::Promise && value.car() != unbound() {
            value = value.car();
        }
        if values::is_function(value) {
            return Ok(value);
        }
    }
    activity::count(Counter::LookupMisses);
    if let Some(cell) = resolve(symbol, enclosure, true)? {
        fill_cache(symbol, enclosure, cell, cache, slot);
    }
    eval::find_called_fun(symbol, env, call)
}

/// [`lookup_function`] for a symbol the frame cannot bind: the frame is not probed.
#[inline(always)]
unsafe fn lookup_enclosing_function(
    symbol: Sexp,
    env: Sexp,
    cache: LookupCache,
    slot: usize,
) -> R<Sexp> {
    if let Ok(enclosure) =
        outside_function_frames(symbol, env.enclosure(), &mut *cache.hints.add(slot))
        && let Some(cell) = cached_cell(symbol, enclosure, cache, slot)
        && !cell.has_gp(gp::ACTIVE_BINDING)
    {
        let mut value = cell.cdr();
        if value.kind() == SexpType::Promise && value.car() != unbound() {
            value = value.car();
        }
        if values::is_function(value) {
            return Ok(value);
        }
    }
    lookup_function(symbol, env, cache, slot, unbound())
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
        LookupCache {
            epochs: values::reals_mut(code.cdr().element(EPOCHS)).as_mut_ptr() as *mut u64,
            caches,
            slots: caches.data() as *const Sexp,
            hints: values::ints_mut(code.cdr().element(HINTS)).as_mut_ptr() as *mut u32,
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
