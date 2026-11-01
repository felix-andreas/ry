//! The tier-0 bytecode interpreter.
//!
//! A closure body compiles to bytecode on its second call. The code replaces the body as a
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
//!   epochs (a double vector), the third the type feedback (a raw vector with one byte per
//!   instruction word, set at the position of the instruction that produced a value), the fourth
//!   the loop table (an integer vector of `start, end, exit, step` quadruples), the fifth the
//!   number of registers the code uses, the sixth the profile counters (an integer vector, see
//!   [`profile`]) and the seventh the optimizing tier's compiled code, `NULL` until it exists.
//! - `tag`: the inline caches, a list with two slots per cache: the enclosure the lookup started
//!   from and the binding cell it found.
//!
//! The optimizing tier plugs in through [`Tier`]. It works on the same register file: a register
//! window is a fixed slice of one preallocated buffer that never moves, so compiled code keeps a
//! pointer to its window, stores its boxed values there (which roots them), and on
//! deoptimization returns [`TierExit::Resume`], and the interpreter continues at any
//! instruction.

use crate::{
    builtins::{Arg, BuiltinDef, Call, Generic, internal},
    conditions, dispatch,
    eval::{self, as_condition},
    interp,
    unwind::{Jump, R, Unwind, error, error_in},
    values::{self, length, missing_arg, nil, symbol_name, symbols, unbound},
};
use heap::{
    Marker,
    object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Sexp, SexpType, gp, is_na_real},
};
use rustc_hash::{FxHashMap, FxHashSet};

/// The internals behind GNU R's `compiler` package. Its code buffer is assembled in R, and
/// `mkCode` turns it into byte code; ry keeps only the buffer's first constant, the source
/// expression, exactly as it reads GNU R byte code from a stream, and compiles that itself.
pub const BUILTINS: &[BuiltinDef] = &[
    internal("bcVersion", 0, |_| unsafe { values::scalar_integer(GNU_R_BYTECODE_VERSION) }),
    internal("mkCode", 2, do_mk_code),
    internal("bcClose", 3, |call| unsafe { eval::make_closure(call.arg(0), call.arg(1), nil(), call.arg(2)) }),
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
        State { registers: Vec::with_capacity(REGISTER_FILE_CAPACITY), calls: FxHashMap::default(), tier: None }
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
    /// Called whenever compiled bytecode starts running, with a fresh register window. Returns
    /// `None` to let the interpreter run it from the start.
    pub enter: unsafe fn(&mut Machine<'_>) -> Option<TierExit>,
    /// Called on a backward jump to `target` once the code's back-edge counter crosses
    /// [`Tier::back_edge_threshold`]. Returns `None` to keep interpreting at `target`.
    pub back_edge: unsafe fn(&mut Machine<'_>, target: usize) -> Option<TierExit>,
    pub back_edge_threshold: i32,
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
    /// The closure whose body `code` is.
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

/// The address of register `index` of the file. Valid while the window that holds it exists.
pub fn register_address(index: usize) -> *mut Sexp {
    // The buffer never reallocates, so this pointer stays valid as the file grows and shrinks.
    unsafe { interp().bytecode.registers.as_mut_ptr().add(index) }
}

/// Grows the register file to `length` registers, filling new ones with `NULL`.
pub fn reserve_registers(length: usize) -> R<()> {
    let registers = &mut interp().bytecode.registers;
    if length > registers.capacity() {
        return Err(error("evaluation nested too deeply: the register file is full"));
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
    // A body with a loop pays for compiling within its first call, as GNU R's JIT assumes.
    if *calls < COMPILE_AFTER_CALLS && !contains_loop(body, LOOP_SEARCH_DEPTH) {
        if state.calls.len() > MAX_TRACKED_BODIES {
            state.calls.clear();
        }
        return eval::eval(body, env);
    }
    state.calls.remove(&body);
    // A body the compiler cannot handle still runs correctly on the AST evaluator.
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

pub unsafe fn is_compiled(function: Sexp) -> bool {
    function.cdr().kind() == SexpType::Bytecode
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
pub const FIRST_CONSTANT: usize = 7;

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

unsafe fn source_of(code: Sexp) -> Sexp {
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
    /// `dst function call`: any other call, with the AST evaluator's argument handling.
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
    /// `dst variable index value call double`: `variable[index] <- value` (or `[[` when
    /// `double` is 1), where `index` is a register that already holds the index's value and the
    /// assignment runs as `<-` would run it.
    SubAssign,
    /// `srcref`: makes the srcref constant current, for a statement of a `{` with source
    /// references.
    SetSrcref,
    /// `function expected target`: jumps unless the function is the primitive `expected`.
    BranchUnlessPrimitive,
    /// `src target`: jumps when the value in `src` is an object (it has a class).
    BranchIfObject,
    /// `dst function call subject`: calls a dispatch-or-eval special such as `[` whose first
    /// argument already evaluated to the object in `subject`, so it dispatches on that object
    /// with the other arguments as promises.
    DispatchSubject,
}

pub const OPS: [Op; 26] = [
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
/// ALTREP.
pub mod feedback {
    pub const INTEGER_SCALAR: u8 = 1 << 0;
    pub const DOUBLE_SCALAR: u8 = 1 << 1;
    pub const LOGICAL_SCALAR: u8 = 1 << 2;
    pub const INTEGER_VECTOR: u8 = 1 << 3;
    pub const DOUBLE_VECTOR: u8 = 1 << 4;
    pub const LOGICAL_VECTOR: u8 = 1 << 5;
    pub const LIST: u8 = 1 << 6;
    pub const OTHER: u8 = 1 << 7;
}

/// The [`feedback`] bit for `value`.
pub unsafe fn value_kind(value: Sexp) -> u8 {
    if values::has_attributes(value) || value.is_altrep() || value.is_array() {
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
}

#[derive(Clone, Copy)]
struct Loop {
    start: u32,
    end: u32,
    exit: u32,
    step: u32,
}

/// Compiles a closure body, or reports that it is better left to the AST evaluator.
unsafe fn compile(body: Sexp, function: Sexp) -> R<Option<Sexp>> {
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
    };
    let result = compiler.allocate();
    compiler.expression(body, result)?;
    compiler.emit(Op::Return, &[result]);
    let heap = crate::heap();
    let words = values::alloc(SexpType::Integer, compiler.code.len())?;
    for (index, word) in compiler.code.iter().enumerate() {
        values::ints_mut(words)[index] = *word as i32;
    }
    let constants = values::alloc(SexpType::List, FIRST_CONSTANT + compiler.constants.len())?;
    heap.set_element(constants, SOURCE, body);
    let epochs = values::alloc(SexpType::Real, compiler.caches as usize)?;
    values::reals_mut(epochs).fill(-1.0);
    heap.set_element(constants, EPOCHS, epochs);
    let feedback = values::alloc(SexpType::Raw, compiler.code.len())?;
    values::raws_mut(feedback).fill(0);
    heap.set_element(constants, FEEDBACK, feedback);
    let profile = values::alloc(SexpType::Integer, profile::LENGTH)?;
    values::ints_mut(profile).fill(0);
    heap.set_element(constants, PROFILE, profile);
    heap.set_element(constants, TIER1, nil());
    let loops = values::alloc(SexpType::Integer, compiler.loops.len() * 4)?;
    for (index, entry) in compiler.loops.iter().enumerate() {
        let slots = &mut values::ints_mut(loops)[index * 4..index * 4 + 4];
        slots.copy_from_slice(&[entry.start as i32, entry.end as i32, entry.exit as i32, entry.step as i32]);
    }
    heap.set_element(constants, LOOPS, loops);
    let registers = values::scalar_integer(compiler.registers as i32)?;
    heap.set_element(constants, REGISTERS, registers);
    for (index, constant) in compiler.constants.iter().enumerate() {
        heap.set_element(constants, FIRST_CONSTANT + index, *constant);
    }
    let caches = values::alloc(SexpType::List, compiler.caches as usize * 2)?;
    let code = values::triple(SexpType::Bytecode, words, constants, caches)?;
    Ok(Some(code))
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
    if collect_bindings(body, &mut symbols_found) { Some(symbols_found) } else { None }
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
        if matches!(&*name, "<-" | "=" | "<<-") && expression.cdr().car().kind() == SexpType::Language {
            return true;
        }
        if matches!(&*name, "eval" | "evalq" | "local" | "with" | "within" | "source" | "sys.function" | "do.call") {
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
            "assign" | "eval" | "evalq" | "delayedAssign" | "makeActiveBinding" | "local" | "sys.function"
                | "environment" | "list2env" | "sys.frame" | "sys.frames" | "attach" | "with" | "within"
                | "source" | "rm" | "remove" | "lockBinding" | "on.exit" | "Recall" | "do.call"
        ) {
            return false;
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
    unsafe fn frame_may_bind(&self, symbol: Sexp) -> bool {
        self.local_symbols.as_ref().is_none_or(|found| found.contains(&symbol))
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
        self.code.push(op as u32);
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
            SexpType::Promise | SexpType::Bytecode | SexpType::Dots => self.fallback(expression, dst),
            _ => {
                let constant = self.constant(expression);
                self.emit(Op::Constant, &[dst, constant]);
                Ok(())
            }
        }
    }

    unsafe fn symbol(&mut self, symbol: Sexp, dst: u32) -> R<()> {
        if symbol == symbols().dots || eval::dot_dot_index(symbol).is_some() || symbol == missing_arg() {
            return self.fallback(symbol, dst);
        }
        let constant = self.constant(symbol);
        let cache = self.cache();
        self.emit(Op::GetVar, &[dst, constant, cache]);
        Ok(())
    }

    unsafe fn fallback(&mut self, expression: Sexp, dst: u32) -> R<()> {
        let constant = self.constant(expression);
        self.emit(Op::Ast, &[dst, constant]);
        Ok(())
    }

    unsafe fn call(&mut self, call: Sexp, dst: u32) -> R<()> {
        let head = call.car();
        if head.kind() != SexpType::Symbol {
            return self.fallback(call, dst);
        }
        let name = symbol_name(head).into_owned();
        if let Some(primitive) = self.base_primitive(head, &name)? {
            let args = values::pairlist_items(call.cdr());
            if self.eager_subject(primitive, &args)? {
                return self.subject_call(call, head, primitive, dst, &args);
            }
            let plain = args.iter().all(|(tag, _)| *tag == nil());
            let inlined = match (name.as_str(), args.len()) {
                ("(", 1) => plain && args[0].1 != symbols().dots,
                ("{", _) | ("if", 2 | 3) | ("while", 2) | ("repeat", 1) | ("return", 0 | 1) => plain,
                ("for", 3) => plain && args[0].1.kind() == SexpType::Symbol,
                ("break" | "next", 0) => !self.open_loops.is_empty(),
                ("<-" | "=", 2) => plain && args[0].1.kind() == SexpType::Symbol,
                ("&&" | "||", 2) => plain,
                ("function", 2..) => true,
                (operator, 2) if plain && ARITHS.iter().any(|(_, symbol)| *symbol == operator) => {
                    args.iter().all(|(_, value)| *value != missing_arg() && *value != symbols().dots)
                }
                _ => false,
            };
            if matches!(name.as_str(), "<-" | "=")
                && args.len() == 2
                && args.iter().all(|(tag, _)| *tag == nil())
                && let Some((double, replacement)) = self.element_assignment_target(args[0].1)?
            {
                return self.element_assignment(call, head, primitive, replacement, double, dst, &args);
            }
            if inlined {
                return self.guarded(call, head, primitive, dst, &name, &args);
            }
        }
        self.general_call(call, head, dst)
    }

    /// For `x[i]` or `x[[i]]` as an assignment target, whether it is `[[` and the base
    /// replacement function, when `x` is a variable and `i` is a constant or a variable that is
    /// never a promise (so evaluating it early is unobservable).
    unsafe fn element_assignment_target(&mut self, target: Sexp) -> R<Option<(bool, Sexp)>> {
        if target.kind() != SexpType::Language || target.car().kind() != SexpType::Symbol {
            return Ok(None);
        }
        let double = match &*symbol_name(target.car()) {
            "[" => false,
            "[[" => true,
            _ => return Ok(None),
        };
        let parts = values::pairlist_items(target.cdr());
        let [(variable_tag, variable), (index_tag, index)] = parts.as_slice() else {
            return Ok(None);
        };
        if *variable_tag != nil() || *index_tag != nil() || variable.kind() != SexpType::Symbol {
            return Ok(None);
        }
        let simple_index = match index.kind() {
            SexpType::Symbol => {
                *index != symbols().dots
                    && *index != missing_arg()
                    && eval::dot_dot_index(*index).is_none()
                    && !self.formals.contains(index)
                    && self.local_symbols.as_ref().is_some_and(|found| found.contains(index))
            }
            SexpType::Integer | SexpType::Real => length(*index) == 1 && !values::has_attributes(*index),
            _ => false,
        };
        if !simple_index {
            return Ok(None);
        }
        let name = if double { "[[<-" } else { "[<-" };
        let replacement = values::install(name)?;
        Ok(self.base_primitive(replacement, name)?.map(|primitive| (double, primitive)))
    }

    /// `x[i] <- value`, guarded by both `<-` and the replacement function.
    #[allow(clippy::too_many_arguments)]
    unsafe fn element_assignment(
        &mut self,
        call: Sexp,
        head: Sexp,
        primitive: Sexp,
        replacement: Sexp,
        double: bool,
        dst: u32,
        args: &[(Sexp, Sexp)],
    ) -> R<()> {
        let target = args[0].1;
        let replacement_symbol = values::install(if double { "[[<-" } else { "[<-" })?;
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
        let index = self.allocate();
        self.expression(args[1].1, value)?;
        let index_expression = target.cdr().cdr().car();
        self.expression(index_expression, index)?;
        let variable = self.constant(target.cdr().car());
        let call_constant = self.constant(call);
        self.emit(Op::SubAssign, &[dst, variable, index, value, call_constant, double as u32]);
        self.release_to(mark);
        let done = self.emit_patchable(Op::Jump, &[0]);
        for guard in guards {
            self.patch(guard);
        }
        self.fallback(call, dst)?;
        self.patch(done);
        Ok(())
    }

    /// The base primitive `symbol` names from the closure's enclosure, if it is one the compiler
    /// knows how to inline.
    unsafe fn base_primitive(&mut self, symbol: Sexp, name: &str) -> R<Option<Sexp>> {
        let inlinable = matches!(
            name,
            "{" | "(" | "if" | "for" | "while" | "repeat" | "break" | "next" | "return" | "<-" | "=" | "&&" | "||" | "function"
                | "[" | "[[" | "[<-" | "[[<-"
        ) || ARITHS.iter().any(|(_, operator)| *operator == name);
        if !inlinable {
            return Ok(None);
        }
        let Some(expected) = crate::builtins::primitive(name) else {
            return Ok(None);
        };
        // A walk that forces no promise: compiling must not run code.
        let heap = crate::heap();
        let mut env = self.enclosure;
        while env != heap.known().empty_env {
            if let Some(cell) = heap.frame_cell(env, symbol) {
                let value = cell.cdr();
                if cell.has_gp(gp::ACTIVE_BINDING) || value.kind() == SexpType::Promise {
                    return Ok(None);
                }
                if values::is_function(value) {
                    return Ok((value == expected).then_some(expected));
                }
            }
            env = env.enclosure();
        }
        Ok(None)
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
        let done = self.emit_patchable(Op::Jump, &[0]);
        self.patch(guard);
        self.fallback(call, dst)?;
        self.patch(done);
        Ok(())
    }

    unsafe fn inline(&mut self, call: Sexp, primitive: Sexp, dst: u32, name: &str, args: &[(Sexp, Sexp)]) -> R<()> {
        match name {
            "{" => {
                if args.is_empty() {
                    let constant = self.constant(nil());
                    self.emit(Op::Constant, &[dst, constant]);
                    // `{}` is visible NULL.
                    self.emit(Op::Visible, &[]);
                }
                let srcrefs = values::get_attr(call, symbols().srcref);
                if srcrefs == nil() {
                    for (_, statement) in args {
                        self.expression(*statement, dst)?;
                    }
                    return Ok(());
                }
                let outer = self.srcref;
                for (index, (_, statement)) in args.iter().enumerate() {
                    self.srcref = eval::statement_srcref(srcrefs, index + 1);
                    let srcref = self.constant(self.srcref);
                    self.emit(Op::SetSrcref, &[srcref]);
                    self.expression(*statement, dst)?;
                }
                self.srcref = outer;
                let srcref = self.constant(outer);
                self.emit(Op::SetSrcref, &[srcref]);
            }
            "(" => {
                self.expression(args[0].1, dst)?;
                self.emit(Op::Visible, &[]);
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
                let loop_index = *self.open_loops.last().ok_or_else(|| error("no loop for break/next"))?;
                // The targets are known once the loop closes; record the operand to patch.
                let position = self.emit_patchable(Op::Jump, &[0]);
                let marker = if name == "break" { BREAK_MARKER } else { NEXT_MARKER };
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
            "<-" | "=" => {
                self.expression(args[1].1, dst)?;
                let symbol = self.constant(args[0].1);
                self.emit(Op::SetVar, &[symbol, dst]);
            }
            "&&" | "||" => {
                let is_and = (name == "&&") as u32;
                let call_constant = self.constant(call);
                let left = self.allocate();
                self.expression(args[0].1, left)?;
                let decided = self.emit_patchable(Op::LogicLeft, &[dst, left, call_constant, is_and, 0]);
                let mark = self.next_register;
                let right = self.allocate();
                self.expression(args[1].1, right)?;
                self.emit(Op::LogicRight, &[dst, left, right, call_constant, is_and]);
                self.release_to(mark);
                self.patch(decided);
                self.release_to(left);
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
                self.emit(Op::Arithmetic, &[arith as u32, dst, left, right, call_constant, function]);
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
        self.loops.push(Loop { start, end: 0, exit: 0, step: 0 });
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
            let op = OPS[self.code[position] as usize];
            let width = operand_count(op);
            if op == Op::Jump {
                let operand = self.code[position + 1];
                if operand & MARKER_MASK != 0 && (operand & !MARKER_MASK) as usize == loop_index {
                    self.code[position + 1] = if operand & BREAK_MARKER != 0 { exit } else { step };
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
        if primitive.kind() != SexpType::Special || !eval::builtin_def(primitive)?.is_dispatch_or_eval() {
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
        Ok(*subject != missing_arg() && *subject != symbols().dots && rest.iter().all(|(_, value)| plain_argument(*value)))
    }

    /// A dispatch-or-eval special call: the subject evaluates first, an object dispatches with
    /// the other arguments as promises, and any other subject calls the special on values, the
    /// path the optimizing tier compiles.
    unsafe fn subject_call(&mut self, call: Sexp, head: Sexp, primitive: Sexp, dst: u32, args: &[(Sexp, Sexp)]) -> R<()> {
        let symbol = self.constant(head);
        let expected = self.constant(primitive);
        let call_constant = self.constant(call);
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
        let object = self.emit_patchable(Op::BranchIfObject, &[base, 0]);
        for (index, (_, value)) in args.iter().enumerate().skip(1) {
            if *value == missing_arg() {
                let constant = self.constant(missing_arg());
                self.emit(Op::Constant, &[base + index as u32, constant]);
            } else {
                self.expression(*value, base + index as u32)?;
            }
        }
        self.emit(Op::CallBuiltin, &[dst, function, call_constant, base, args.len() as u32]);
        let called = self.emit_patchable(Op::Jump, &[0]);
        self.patch(object);
        self.emit(Op::DispatchSubject, &[dst, function, call_constant, base]);
        let dispatched = self.emit_patchable(Op::Jump, &[0]);
        self.patch(generic);
        self.emit(Op::CallGeneric, &[dst, function, call_constant]);
        self.patch(called);
        self.patch(dispatched);
        self.release_to(mark);
        Ok(())
    }

    unsafe fn general_call(&mut self, call: Sexp, head: Sexp, dst: u32) -> R<()> {
        let symbol = self.constant(head);
        let call_constant = self.constant(call);
        let cache = self.cache();
        let mark = self.next_register;
        let function = self.allocate();
        self.emit(Op::GetFun, &[function, symbol, cache, call_constant]);
        let args = values::pairlist_items(call.cdr());
        // A register holds an argument's value without counting as a reference, so a later
        // argument that modifies a variable in place would change an earlier argument's value.
        let simple = args.iter().all(|(_, value)| *value != symbols().dots)
            && !args.iter().skip(1).any(|(_, value)| may_modify_in_place(*value));
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
            self.emit(Op::CallBuiltin, &[dst, function, call_constant, base, args.len() as u32]);
            let done = self.emit_patchable(Op::Jump, &[0]);
            self.patch(generic);
            self.emit(Op::CallGeneric, &[dst, function, call_constant]);
            self.patch(done);
        } else {
            self.emit(Op::CallGeneric, &[dst, function, call_constant]);
        }
        self.release_to(mark);
        Ok(())
    }
}

const BREAK_MARKER: u32 = 1 << 30;
const NEXT_MARKER: u32 = 1 << 29;
const MARKER_MASK: u32 = BREAK_MARKER | NEXT_MARKER;

pub fn operand_count(op: Op) -> usize {
    match op {
        Op::Constant => 2,
        Op::GetVar => 3,
        Op::GetFun => 4,
        Op::SetVar => 2,
        Op::BranchUnlessBuiltin => 2,
        Op::CallBuiltin => 5,
        Op::CallGeneric => 3,
        Op::Ast => 2,
        Op::Guard => 5,
        Op::Jump => 1,
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
        Op::SetSrcref => 1,
        Op::BranchUnlessPrimitive => 3,
        Op::BranchIfObject => 2,
        Op::DispatchSubject => 4,
    }
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
        LoopState { index, count, cell: None }
    }
}

/// The number of `for` loops in `code`.
pub unsafe fn loop_count(code: Sexp) -> usize {
    length(code.cdr().element(LOOPS)) / 4
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
    profile[profile::ENTRIES] = profile[profile::ENTRIES].saturating_add(1);
    let mut loops = vec![LoopState::default(); loop_count(code)];
    let mut machine = Machine { function, code, env, base, mark, loops: &mut loops };
    let entered = match interp().bytecode.tier {
        Some(tier) => (tier.enter)(&mut machine),
        None => None,
    };
    let result = match entered {
        Some(TierExit::Finished(result)) => result,
        Some(TierExit::Resume { pc, pending }) => match pending {
            Some(unwind) => continue_after(&mut machine, pc, unwind).and_then(|pc| execute_code(&mut machine, pc)),
            None => execute_code(&mut machine, pc),
        },
        None => execute_code(&mut machine, 0),
    };
    interp().bytecode.registers.truncate(base);
    match result {
        Err(Unwind::Error(failure)) => Err(conditions::signal_error(failure)),
        other => other,
    }
}

unsafe fn execute_code(machine: &mut Machine<'_>, mut pc: usize) -> R<Sexp> {
    loop {
        let at = pc;
        match step(machine, &mut pc) {
            Ok(Some(value)) => return Ok(value),
            Ok(None) => {
                if pc < at
                    && let Some(tier) = interp().bytecode.tier
                {
                    let profile = values::ints_mut(machine.code.cdr().element(PROFILE));
                    profile[profile::BACK_EDGES] = profile[profile::BACK_EDGES].saturating_add(1);
                    if profile[profile::BACK_EDGES] >= tier.back_edge_threshold {
                        profile[profile::BACK_EDGES] = 0;
                        match (tier.back_edge)(machine, pc) {
                            Some(TierExit::Finished(result)) => return result,
                            Some(TierExit::Resume { pc: at, pending: Some(unwind) }) => pc = continue_after(machine, at, unwind)?,
                            Some(TierExit::Resume { pc: at, pending: None }) => pc = at,
                            None => {}
                        }
                    }
                }
            }
            Err(unwind) => pc = continue_after(machine, at, unwind)?,
        }
    }
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
    let mut next = pc;
    step(machine, &mut next).map(|_| ())
}

/// What `GetVar` at `pc` reads, and whether the binding is in the activation's own frame.
pub unsafe fn load_variable(code: Sexp, env: Sexp, pc: usize) -> R<(Sexp, bool)> {
    let words = values::ints(code.car());
    let constants = code.cdr();
    let symbol = constants.element(words[pc + 2] as usize);
    let local = crate::heap().frame_cell(env, symbol).is_some();
    let value = lookup_variable(symbol, env, constants, code.tag(), words[pc + 3] as usize)?;
    record_feedback(constants, pc, value);
    Ok((value, local))
}

/// The binding cell inline cache `slot` of `code` holds for lookups from `enclosure`, if it is
/// still valid.
pub unsafe fn cached_binding(code: Sexp, enclosure: Sexp, slot: usize) -> Option<Sexp> {
    cached_cell(enclosure, code.cdr(), code.tag(), slot)
}

/// Runs one instruction. `Some(value)` means the body returned.
unsafe fn step(machine: &mut Machine<'_>, pc: &mut usize) -> R<Option<Sexp>> {
    let env = machine.env;
    let base = machine.base;
    let words = machine.code.car();
    let constants = machine.code.cdr();
    let caches = machine.code.tag();
    let code = values::ints(words);
    let at = *pc;
    let operand = |index: usize| code[at + 1 + index] as usize;
    let register = |index: usize| interp().bytecode.registers[base + index];
    let set = |index: usize, value: Sexp| interp().bytecode.registers[base + index] = value;
    let op = OPS[code[at] as usize];
    let next = at + 1 + operand_count(op);
    match op {
        Op::Constant => {
            set(operand(0), constants.element(operand(1)));
            interp().visible = true;
        }
        Op::GetVar => {
            // Visible before the lookup, so that forcing a promise to `invisible()` leaves the
            // value invisible, as `withCallingHandlers(expr)` relies on.
            interp().visible = true;
            let symbol = constants.element(operand(1));
            let value = lookup_variable(symbol, env, constants, caches, operand(2))?;
            record_feedback(constants, at, value);
            set(operand(0), value);
        }
        Op::SetVar => {
            let symbol = constants.element(operand(0));
            let value = register(operand(1));
            eval::assign_symbol(symbol, value, env, false)?;
            interp().visible = false;
        }
        Op::GetFun => {
            let symbol = constants.element(operand(1));
            let call = constants.element(operand(3));
            let function = lookup_function(symbol, env, constants, caches, operand(2), call)?;
            set(operand(0), function);
        }
        Op::BranchUnlessBuiltin => {
            if register(operand(0)).kind() != SexpType::Builtin {
                *pc = operand(1);
                return Ok(None);
            }
        }
        Op::CallBuiltin => {
            let function = register(operand(1));
            let call = constants.element(operand(2));
            let first = operand(3);
            let count = operand(4);
            let definition = eval::builtin_def(function)?;
            let mut args = Vec::with_capacity(count);
            let mut cell = call.cdr();
            for index in 0..count {
                let value = register(first + index);
                // Only an empty argument is an error: a promise may well evaluate to the empty
                // symbol, as `X[[i]]` does for a formal without a default.
                if cell.car() == missing_arg() && !definition.keep_missing {
                    return Err(error_in(call, format!("argument {} is empty", index + 1)));
                }
                args.push(Arg { tag: cell.tag(), value });
                cell = cell.cdr();
            }
            let value = call_builtin(definition, call, function, &args, env)?;
            record_feedback(constants, at, value);
            set(operand(0), value);
        }
        Op::CallGeneric => {
            let function = register(operand(1));
            let call = constants.element(operand(2));
            let value = eval::apply_call(call, function, call.cdr(), env)?;
            record_feedback(constants, at, value);
            set(operand(0), value);
        }
        Op::BranchUnlessPrimitive => {
            if register(operand(0)) != constants.element(operand(1)) {
                *pc = operand(2);
                return Ok(None);
            }
        }
        Op::BranchIfObject => {
            if register(operand(0)).is_object() {
                *pc = operand(1);
                return Ok(None);
            }
        }
        Op::DispatchSubject => {
            let function = register(operand(1));
            let call = constants.element(operand(2));
            let definition = eval::builtin_def(function)?;
            let value = match dispatch::dispatch_evaluated_subject(definition, call, function, call.cdr(), register(operand(3)), env)? {
                Ok(value) => value,
                Err(args) => call_builtin(definition, call, function, &args, env)?,
            };
            record_feedback(constants, at, value);
            set(operand(0), value);
        }
        Op::Ast => {
            let expression = constants.element(operand(1));
            let value = eval::eval(expression, env)?;
            record_feedback(constants, at, value);
            set(operand(0), value);
        }
        Op::Guard => {
            let symbol = constants.element(operand(0));
            let expected = constants.element(operand(1));
            let found = if operand(3) == 0 {
                lookup_enclosing_function(symbol, env, constants, caches, operand(2))
            } else {
                lookup_function(symbol, env, constants, caches, operand(2), unbound())
            };
            if !matches!(found, Ok(function) if function == expected) {
                *pc = operand(4);
                return Ok(None);
            }
        }
        Op::Jump => {
            let target = operand(0);
            if target < at {
                crate::heap().release_handles(machine.mark);
                crate::unwind::poll_interrupt()?;
            }
            *pc = target;
            return Ok(None);
        }
        Op::JumpUnless => {
            let call = constants.element(operand(1));
            if !as_condition(register(operand(0)), call)? {
                *pc = operand(2);
                return Ok(None);
            }
        }
        Op::Arithmetic => {
            let left = register(operand(2));
            let right = register(operand(3));
            let value = match fast_arithmetic(ARITHS[operand(0)].0, left, right)? {
                Some(value) => value,
                None => {
                    let call = constants.element(operand(4));
                    let function = constants.element(operand(5));
                    let definition = eval::builtin_def(function)?;
                    let args = [Arg { tag: nil(), value: left }, Arg { tag: nil(), value: right }];
                    call_builtin(definition, call, function, &args, env)?
                }
            };
            record_feedback(constants, at, value);
            set(operand(1), value);
            interp().visible = true;
        }
        Op::Return => return Ok(Some(register(operand(0)))),
        Op::ForInit => {
            let call = constants.element(operand(2));
            let mut sequence = register(operand(1));
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
            // The body must see the sequence as shared, so that modifying the variable it came
            // from copies it instead of changing the elements still to come.
            sequence.increment_refcount();
            record_feedback(constants, at, sequence);
            set(operand(1), sequence);
            machine.loops[operand(0)] = LoopState { index: 0, count, cell: Some(sequence) };
        }
        Op::ForStep => {
            let state = &mut machine.loops[operand(0)];
            if state.index >= state.count {
                *pc = operand(3);
                return Ok(None);
            }
            let sequence = register(operand(1));
            let element = match sequence.kind() {
                SexpType::List | SexpType::Expression => sequence.element(state.index),
                SexpType::Pairlist | SexpType::Language => {
                    let cell = state.cell.unwrap_or(sequence);
                    state.cell = Some(cell.cdr());
                    cell.car()
                }
                _ => crate::subset::vector_element(sequence, state.index)?,
            };
            state.index += 1;
            let variable = constants.element(operand(2));
            eval::define(variable, element, env)?;
        }
        Op::ForEnd => register(operand(0)).decrement_refcount(),
        Op::Invisible => interp().visible = false,
        Op::Visible => interp().visible = true,
        Op::SetSrcref => interp().srcref = constants.element(operand(0)),
        Op::LogicLeft => {
            let call = constants.element(operand(2));
            let is_and = operand(3) == 1;
            let left = eval::logical_operand_of(register(operand(1)), call, if is_and { "&&" } else { "||" }, "x")?;
            if (is_and && left == 0) || (!is_and && left == 1) {
                set(operand(0), values::scalar_logical(left)?);
                interp().visible = true;
                *pc = operand(4);
                return Ok(None);
            }
            set(operand(1), values::scalar_logical(left)?);
        }
        Op::LogicRight => {
            let call = constants.element(operand(3));
            let is_and = operand(4) == 1;
            let left = values::ints(register(operand(1)))[0];
            let right = eval::logical_operand_of(register(operand(2)), call, if is_and { "&&" } else { "||" }, "y")?;
            set(operand(0), values::scalar_logical(logic_result(is_and, left, right))?);
            interp().visible = true;
        }
        Op::Closure => {
            let formals = constants.element(operand(1));
            let body = constants.element(operand(2));
            let srcref = constants.element(operand(3));
            set(operand(0), eval::make_closure(formals, body, srcref, env)?);
            interp().visible = true;
        }
        Op::SubAssign => {
            let call = constants.element(operand(4));
            let value = register(operand(3));
            eval::complex_assign(call, call.cdr().car(), value, env, false)?;
            set(operand(0), value);
            interp().visible = false;
        }
    }
    *pc = next;
    Ok(None)
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

unsafe fn call_builtin(definition: &'static BuiltinDef, call: Sexp, function: Sexp, args: &[Arg], env: Sexp) -> R<Sexp> {
    if definition.generic != Generic::None
        && let Some(value) = dispatch::try_builtin_dispatch(definition, call, function, args, env)?
    {
        return Ok(value);
    }
    eval::call_builtin(definition, call, function, args, env)
}

unsafe fn record_feedback(constants: Sexp, pc: usize, value: Sexp) {
    let feedback = constants.element(FEEDBACK);
    values::raws_mut(feedback)[pc] |= value_kind(value);
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
            if integer == NA_INTEGER { NA_REAL } else { integer as f64 }
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
unsafe fn lookup_variable(symbol: Sexp, env: Sexp, constants: Sexp, caches: Sexp, slot: usize) -> R<Sexp> {
    let heap = crate::heap();
    if let Some(cell) = heap.frame_cell(env, symbol) {
        let value = cell.cdr();
        let plain = !cell.has_gp(gp::ACTIVE_BINDING)
            && value.kind() != SexpType::Promise
            && value != missing_arg()
            && value != unbound();
        return if plain { Ok(value) } else { eval::eval_symbol(symbol, env) };
    }
    let enclosure = env.enclosure();
    if let Some(cell) = cached_cell(enclosure, constants, caches, slot)
        && !cell.has_gp(gp::ACTIVE_BINDING)
    {
        let value = cell.cdr();
        if value.kind() == SexpType::Promise {
            return eval::eval_symbol(symbol, env);
        }
        if value != missing_arg() && value != unbound() {
            return Ok(value);
        }
    }
    if let Some(cell) = resolve(symbol, enclosure, false)? {
        fill_cache(enclosure, cell, constants, caches, slot);
    }
    eval::eval_symbol(symbol, env)
}

/// A function's value, with GNU R's rule that bindings to non-functions are skipped.
unsafe fn lookup_function(
    symbol: Sexp,
    env: Sexp,
    constants: Sexp,
    caches: Sexp,
    slot: usize,
    call: Sexp,
) -> R<Sexp> {
    let heap = crate::heap();
    if heap.frame_cell(env, symbol).is_some() {
        return eval::find_called_fun(symbol, env, call);
    }
    let enclosure = env.enclosure();
    if let Some(cell) = cached_cell(enclosure, constants, caches, slot)
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
    if let Some(cell) = resolve(symbol, enclosure, true)? {
        fill_cache(enclosure, cell, constants, caches, slot);
    }
    eval::find_called_fun(symbol, env, call)
}

/// [`lookup_function`] for a symbol the frame cannot bind: the frame is not probed.
unsafe fn lookup_enclosing_function(symbol: Sexp, env: Sexp, constants: Sexp, caches: Sexp, slot: usize) -> R<Sexp> {
    let enclosure = env.enclosure();
    if let Some(cell) = cached_cell(enclosure, constants, caches, slot)
        && !cell.has_gp(gp::ACTIVE_BINDING)
    {
        let value = cell.cdr();
        if values::is_function(value) {
            return Ok(value);
        }
    }
    lookup_function(symbol, env, constants, caches, slot, unbound())
}

unsafe fn cached_cell(enclosure: Sexp, constants: Sexp, caches: Sexp, slot: usize) -> Option<Sexp> {
    let epochs = constants.element(EPOCHS);
    if values::reals(epochs)[slot] != crate::heap().binding_epoch() as f64 {
        return None;
    }
    if caches.element(slot * 2) != enclosure {
        return None;
    }
    Some(caches.element(slot * 2 + 1))
}

unsafe fn fill_cache(enclosure: Sexp, cell: Sexp, constants: Sexp, caches: Sexp, slot: usize) {
    let heap = crate::heap();
    heap.set_element(caches, slot * 2, enclosure);
    heap.set_element(caches, slot * 2 + 1, cell);
    let epochs = constants.element(EPOCHS);
    values::reals_mut(epochs)[slot] = heap.binding_epoch() as f64;
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
        if let Some(cell) = heap.frame_cell(env, symbol) {
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
