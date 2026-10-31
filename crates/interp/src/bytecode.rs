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
//! (type feedback), which is what a later optimizing tier specializes on.
//!
//! A `BCODESXP` holds:
//! - `car`: the instruction words, an integer vector.
//! - `cdr`: the constants, a list. The first is the source expression, the second the cache
//!   epochs (a double vector), the third the type feedback (a raw vector) and the fourth the
//!   loop table (an integer vector of `start, end, exit, step` quadruples), and the fifth the
//!   number of registers the code uses.
//! - `tag`: the inline caches, a list with two slots per cache: the enclosure the lookup started
//!   from and the binding cell it found.

use crate::{
    builtins::{Arg, BuiltinDef, Generic},
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

pub const BUILTINS: &[BuiltinDef] = &[];

/// The bytecode tier's session state: the register file and the call counts that decide when
/// a closure compiles.
#[derive(Default)]
pub struct State {
    registers: Vec<Sexp>,
    /// Calls seen per uncompiled body. Keys are only compared, never dereferenced, so a body
    /// that died and whose address was reused merely compiles one call early.
    calls: FxHashMap<Sexp, u32>,
}

impl State {
    pub(crate) unsafe fn trace(&self, marker: &mut Marker<'_>) {
        for &register in &self.registers {
            marker.mark(register);
        }
    }
}

/// Runs a byte-code object that appears as an expression, which only happens when code reads a
/// compiled body directly and evaluates it.
pub unsafe fn execute(code: Sexp, env: Sexp) -> R<Sexp> {
    eval::eval(source_of(code), env)
}

/// Runs a closure's body, compiling it on its second call.
pub unsafe fn run_body(function: Sexp, body: Sexp, env: Sexp) -> R<Sexp> {
    if body.kind() == SexpType::Bytecode {
        return run(body, env);
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
    match compile(body, function.car(), function.tag()) {
        Ok(Some(code)) => {
            crate::heap().set_cdr(function, code);
            run(code, env)
        }
        Ok(None) | Err(_) => eval::eval(body, env),
    }
}

/// Records that `symbol` was bound in `env`. The heap's binding epoch already tracks what the
/// inline caches depend on, so nothing else is needed.
pub unsafe fn note_binding(symbol: Sexp, env: Sexp) {
    let _ = (symbol, env);
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

const SOURCE: usize = 0;
const EPOCHS: usize = 1;
const FEEDBACK: usize = 2;
const LOOPS: usize = 3;
const REGISTERS: usize = 4;
const FIRST_CONSTANT: usize = 5;

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
enum Op {
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
    /// `operator dst left right call function feedback`
    Arithmetic,
    /// `src`
    Return,
    /// `loop src call`: prepares the sequence in `src` for iteration.
    ForInit,
    /// `loop src symbol target`: binds the next element, or jumps to `target` when done.
    ForStep,
    Invisible,
    Visible,
    /// `dst src call is_and target`: the left operand of `&&` or `||`; jumps to `target` with
    /// the result in `dst` when it decides the result.
    LogicLeft,
    /// `dst left src call is_and`
    LogicRight,
    /// `dst formals body`
    Closure,
}

const OPS: [Op; 20] = [
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
    Op::Invisible,
    Op::Visible,
    Op::LogicLeft,
    Op::LogicRight,
    Op::Closure,
];

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum Arith {
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

const ARITHS: [(Arith, &str); 11] = [
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

/// Type feedback bits an arithmetic site records about its operands.
mod feedback {
    pub const INTEGER_SCALAR: u8 = 1 << 0;
    pub const DOUBLE_SCALAR: u8 = 1 << 1;
    pub const OTHER: u8 = 1 << 2;
}

// The compiler.

struct Compiler {
    code: Vec<u32>,
    constants: Vec<Sexp>,
    caches: u32,
    feedback: u32,
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
}

#[derive(Clone, Copy)]
struct Loop {
    start: u32,
    end: u32,
    exit: u32,
    step: u32,
}

/// Compiles a closure body, or reports that it is better left to the AST evaluator.
unsafe fn compile(body: Sexp, formals: Sexp, enclosure: Sexp) -> R<Option<Sexp>> {
    let mut compiler = Compiler {
        local_symbols: local_symbols(body, formals),
        code: Vec::new(),
        constants: Vec::new(),
        caches: 0,
        feedback: 0,
        loops: Vec::new(),
        open_loops: Vec::new(),
        registers: 0,
        next_register: 0,
        enclosure,
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
    let feedback = values::alloc(SexpType::Raw, compiler.feedback as usize)?;
    values::raws_mut(feedback).fill(0);
    heap.set_element(constants, FEEDBACK, feedback);
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
unsafe fn local_symbols(body: Sexp, formals: Sexp) -> Option<FxHashSet<Sexp>> {
    let mut symbols_found = FxHashSet::default();
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        symbols_found.insert(formal.tag());
        formal = formal.cdr();
    }
    if collect_bindings(body, &mut symbols_found) { Some(symbols_found) } else { None }
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

    fn feedback_slot(&mut self) -> u32 {
        self.feedback += 1;
        self.feedback - 1
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
            let plain = args.iter().all(|(tag, _)| *tag == nil());
            let inlined = match (name.as_str(), args.len()) {
                ("{", _) | ("(", 1) | ("if", 2 | 3) | ("while", 2) | ("repeat", 1) | ("return", 0 | 1) => plain,
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
            if inlined {
                return self.guarded(call, head, primitive, dst, &name, &args);
            }
        }
        self.general_call(call, head, dst)
    }

    /// The base primitive `symbol` names from the closure's enclosure, if it is one the compiler
    /// knows how to inline.
    unsafe fn base_primitive(&mut self, symbol: Sexp, name: &str) -> R<Option<Sexp>> {
        let inlinable = matches!(
            name,
            "{" | "(" | "if" | "for" | "while" | "repeat" | "break" | "next" | "return" | "<-" | "=" | "&&" | "||" | "function"
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
                for (_, statement) in args {
                    self.expression(*statement, dst)?;
                }
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
                self.emit(Op::Closure, &[dst, formals, body]);
            }
            operator => {
                let arith = ARITHS
                    .iter()
                    .find(|(_, symbol)| *symbol == operator)
                    .map(|(arith, _)| *arith)
                    .ok_or_else(|| error("unknown operator"))?;
                let mark = self.next_register;
                let left = self.allocate();
                let right = self.allocate();
                self.expression(args[0].1, left)?;
                self.expression(args[1].1, right)?;
                let call_constant = self.constant(call);
                let function = self.constant(primitive);
                let slot = self.feedback_slot();
                self.emit(Op::Arithmetic, &[arith as u32, dst, left, right, call_constant, function, slot]);
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

    unsafe fn general_call(&mut self, call: Sexp, head: Sexp, dst: u32) -> R<()> {
        let symbol = self.constant(head);
        let call_constant = self.constant(call);
        let cache = self.cache();
        let mark = self.next_register;
        let function = self.allocate();
        self.emit(Op::GetFun, &[function, symbol, cache]);
        let args = values::pairlist_items(call.cdr());
        let simple = args.iter().all(|(_, value)| *value != symbols().dots);
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

fn operand_count(op: Op) -> usize {
    match op {
        Op::Constant => 2,
        Op::GetVar | Op::GetFun => 3,
        Op::SetVar => 2,
        Op::BranchUnlessBuiltin => 2,
        Op::CallBuiltin => 5,
        Op::CallGeneric => 3,
        Op::Ast => 2,
        Op::Guard => 5,
        Op::Jump => 1,
        Op::JumpUnless => 3,
        Op::Arithmetic => 7,
        Op::Return => 1,
        Op::ForInit => 3,
        Op::ForStep => 4,
        Op::Invisible | Op::Visible => 0,
        Op::LogicLeft => 5,
        Op::LogicRight => 5,
        Op::Closure => 3,
    }
}

// The interpreter.

/// One `for` loop's position in its sequence.
#[derive(Clone, Copy, Default)]
struct ForState {
    index: usize,
    count: usize,
    /// The next cell, for a pairlist sequence.
    cell: Option<Sexp>,
}

unsafe fn run(code: Sexp, env: Sexp) -> R<Sexp> {
    let heap = crate::heap();
    heap.root(code);
    let mark = heap.handle_mark();
    let register_count = values::ints(code.cdr().element(REGISTERS))[0] as usize;
    let state = &mut interp().bytecode;
    let base = state.registers.len();
    state.registers.resize(base + register_count, nil());
    let result = execute_code(code, env, base, mark);
    interp().bytecode.registers.truncate(base);
    match result {
        Err(Unwind::Error(failure)) => Err(conditions::signal_error(failure)),
        other => other,
    }
}

unsafe fn execute_code(code: Sexp, env: Sexp, base: usize, mark: usize) -> R<Sexp> {
    let words = code.car();
    let constants = code.cdr();
    let caches = code.tag();
    let loops = constants.element(LOOPS);
    let mut for_states: Vec<ForState> = vec![ForState::default(); length(loops) / 4];
    let mut pc = 0usize;
    loop {
        match step(words, constants, caches, env, base, mark, &mut pc, &mut for_states) {
            Ok(Some(value)) => return Ok(value),
            Ok(None) => {}
            Err(Unwind::Jump(jump)) => {
                let target = match *jump {
                    Jump::Break { env: target } if target == env => loop_target(loops, pc, true),
                    Jump::Next { env: target } if target == env => loop_target(loops, pc, false),
                    _ => None,
                };
                match target {
                    Some(target) => {
                        crate::heap().release_handles(mark);
                        pc = target;
                    }
                    None => return Err(Unwind::Jump(jump)),
                }
            }
            Err(other) => return Err(other),
        }
    }
}

/// Where a `break` (or `next`) that unwound out of the instruction at `pc` continues: the exit
/// (or step) of the innermost loop containing `pc`.
unsafe fn loop_target(loops: Sexp, pc: usize, is_break: bool) -> Option<usize> {
    let table = values::ints(loops);
    let mut best: Option<(usize, usize)> = None;
    for entry in table.chunks_exact(4) {
        let (start, end) = (entry[0] as usize, entry[1] as usize);
        if start <= pc && pc < end && best.is_none_or(|(best_start, _)| start >= best_start) {
            best = Some((start, if is_break { entry[2] } else { entry[3] } as usize));
        }
    }
    best.map(|(_, target)| target)
}

/// Runs one instruction. `Some(value)` means the body returned.
#[allow(clippy::too_many_arguments)]
unsafe fn step(
    words: Sexp,
    constants: Sexp,
    caches: Sexp,
    env: Sexp,
    base: usize,
    mark: usize,
    pc: &mut usize,
    for_states: &mut [ForState],
) -> R<Option<Sexp>> {
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
            // Set first: forcing a promise leaves the visibility of its expression, as in
            // `f <- function(x) { 1; x }; f(invisible(1))`.
            interp().visible = true;
            let symbol = constants.element(operand(1));
            let value = lookup_variable(symbol, env, constants, caches, operand(2))?;
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
            let function = lookup_function(symbol, env, constants, caches, operand(2))?;
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
                if value == missing_arg() && !definition.keep_missing {
                    return Err(error_in(call, format!("argument {} is empty", index + 1)));
                }
                args.push(Arg { tag: cell.tag(), value });
                cell = cell.cdr();
            }
            let value = call_builtin(definition, call, function, &args, env)?;
            set(operand(0), value);
        }
        Op::CallGeneric => {
            let function = register(operand(1));
            let call = constants.element(operand(2));
            let value = eval::apply_call(call, function, call.cdr(), env)?;
            set(operand(0), value);
        }
        Op::Ast => {
            let expression = constants.element(operand(1));
            let value = eval::eval(expression, env)?;
            set(operand(0), value);
        }
        Op::Guard => {
            let symbol = constants.element(operand(0));
            let expected = constants.element(operand(1));
            let found = if operand(3) == 0 {
                lookup_enclosing_function(symbol, env, constants, caches, operand(2))
            } else {
                lookup_function(symbol, env, constants, caches, operand(2))
            };
            if !matches!(found, Ok(function) if function == expected) {
                *pc = operand(4);
                return Ok(None);
            }
        }
        Op::Jump => {
            let target = operand(0);
            if target < at {
                crate::heap().release_handles(mark);
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
            record_feedback(constants, operand(6), left, right);
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
            set(operand(1), sequence);
            for_states[operand(0)] = ForState { index: 0, count, cell: Some(sequence) };
        }
        Op::ForStep => {
            let state = &mut for_states[operand(0)];
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
        Op::Invisible => interp().visible = false,
        Op::Visible => interp().visible = true,
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
            let result = if is_and {
                if right == 0 { 0 } else if left == NA_LOGICAL || right == NA_LOGICAL { NA_LOGICAL } else { 1 }
            } else if right == 1 {
                1
            } else if left == NA_LOGICAL || right == NA_LOGICAL {
                NA_LOGICAL
            } else {
                0
            };
            set(operand(0), values::scalar_logical(result)?);
            interp().visible = true;
        }
        Op::Closure => {
            let formals = constants.element(operand(1));
            let body = constants.element(operand(2));
            set(operand(0), eval::make_closure(formals, body, env)?);
            interp().visible = true;
        }
    }
    *pc = next;
    Ok(None)
}

unsafe fn call_builtin(definition: &'static BuiltinDef, call: Sexp, function: Sexp, args: &[Arg], env: Sexp) -> R<Sexp> {
    if definition.generic != Generic::None
        && let Some(value) = dispatch::try_builtin_dispatch(definition, call, function, args, env)?
    {
        return Ok(value);
    }
    eval::call_builtin(definition, call, function, args, env)
}

unsafe fn record_feedback(constants: Sexp, slot: usize, left: Sexp, right: Sexp) {
    let kind_bits = |value: Sexp| {
        if values::has_attributes(value) || value.length() != 1 {
            feedback::OTHER
        } else {
            match value.kind() {
                SexpType::Integer => feedback::INTEGER_SCALAR,
                SexpType::Real => feedback::DOUBLE_SCALAR,
                _ => feedback::OTHER,
            }
        }
    };
    let feedback = constants.element(FEEDBACK);
    values::raws_mut(feedback)[slot] |= kind_bits(left) | kind_bits(right);
}

/// Arithmetic and comparison on two attribute-free scalar doubles or integers. `None` sends the
/// operation to the builtin, which handles everything else (and the warnings).
unsafe fn fast_arithmetic(operator: Arith, left: Sexp, right: Sexp) -> R<Option<Sexp>> {
    let scalar = |value: Sexp| {
        matches!(value.kind(), SexpType::Real | SexpType::Integer)
            && value.length() == 1
            && !values::has_attributes(value)
            && !value.is_altrep()
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
unsafe fn lookup_function(symbol: Sexp, env: Sexp, constants: Sexp, caches: Sexp, slot: usize) -> R<Sexp> {
    let heap = crate::heap();
    if heap.frame_cell(env, symbol).is_some() {
        return eval::find_fun(symbol, env);
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
    eval::find_fun(symbol, env)
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
    lookup_function(symbol, env, constants, caches, slot)
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
