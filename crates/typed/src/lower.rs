//! Lowering a typed closure's body to the typed IR.
//!
//! The lowering reads the closure's body as R code and gives every expression a type from the
//! argument types the function is compiled for, the way R would compute it: `i %% 5L` of two
//! integers is an integer, `seq_len(n)` a vector of integers, `paste0("a", i)` a string. A local
//! keeps one type for the whole body, and an expression whose type the lowering cannot determine,
//! or a call it does not know, refuses the function, which then runs as before.
//!
//! Calls of base and standard library functions become IR operations, calls of other typed
//! closures become direct calls of their compiled code, and a local function or a function
//! literal passed to `lapply` or `vapply` is inlined where it is called, reading the enclosing
//! function's locals in place, which is exactly what its environment would give it in R since
//! it never escapes.

use {
    crate::{
        Refusal,
        ir::{
            Arith, BlockId, Builder, Compare, Constant, Item, Local, Math, Op, Reduce, Terminator,
            Text, Type,
        },
        values,
    },
    heap::object::{Sexp, SexpType},
    interp::values as rvalues,
    rustc_hash::FxHashMap,
    smallvec::SmallVec,
};

/// A function lookup the compiled code assumes keeps finding the same function.
#[derive(Clone, Copy, Debug)]
pub struct Speculation {
    pub symbol: Sexp,
    pub env: Sexp,
    pub cell: Sexp,
    pub function: Sexp,
}

/// What lowering needs from the compiler driver: the compiled form of the typed closures the
/// body calls.
pub trait Callees {
    /// The index of the compiled `function` for arguments of these types, and its result type.
    /// `None` for the function being lowered, when the call is recursive: its result type is
    /// then `guess`.
    unsafe fn callee(&mut self, function: Sexp, params: &[Type]) -> Result<(usize, Type), Refusal>;
}

/// The lowered function and the lookups it assumes.
pub struct Lowered {
    pub function: crate::ir::Function,
    pub speculations: Vec<Speculation>,
}

/// The index [`Op::CallTyped`] uses for the function calling itself.
pub const SELF: usize = usize::MAX;

/// How deeply local functions inline into each other.
const MAX_INLINE_DEPTH: usize = 8;
/// How deeply expressions nest, which bounds the lowering's own recursion.
const MAX_NESTING: usize = 256;

/// Lowers `closure` for arguments of `params`, assuming a recursive call returns `guess`.
pub unsafe fn lower(
    closure: Sexp,
    params: &[Type],
    guess: Type,
    callees: &mut dyn Callees,
) -> Result<Lowered, Refusal> {
    let formals = rvalues::pairlist_items(closure.car());
    if formals.len() != params.len() {
        return Err(Refusal("the call does not supply every formal positionally"));
    }
    let body = interp::bytecode::closure_body(closure);
    if tail_invisible(body) {
        return Err(Refusal("the body's value may be invisible"));
    }
    let name = String::from("typed");
    let mut lowering = Lowering {
        builder: Builder::new(name, params),
        closure,
        env: closure.tag(),
        params: SmallVec::from_slice(params),
        guess,
        callees,
        scopes: vec![Scope::default()],
        loops: Vec::new(),
        inlined: Vec::new(),
        speculations: Vec::new(),
        nesting: 0,
    };
    let mut assigned = Vec::new();
    assigned_symbols(body, &mut assigned);
    for (index, &(symbol, _)) in formals.iter().enumerate() {
        if symbol == rvalues::symbols().dots {
            return Err(Refusal("a function with `...`"));
        }
        let param = Local(index as u32);
        let variable = if assigned.contains(&symbol) {
            // An assigned parameter gets an owned copy, since the parameter only borrows.
            let ty = params[index];
            lowering.builder.emit(ty, Op::Copy(param))
        } else {
            param
        };
        lowering.bind(symbol, Binding::Value(variable));
    }
    let value = lowering.expression(body)?;
    if let Some(value) = value {
        let result = lowering.builder.ty(value);
        lowering.builder.function.result = result;
        lowering.builder.terminate(Terminator::Return(value));
    } else {
        lowering.builder.function.result = guess;
    }
    Ok(Lowered {
        function: lowering.builder.function,
        speculations: lowering.speculations,
    })
}

/// What a name in scope stands for.
#[derive(Clone, Copy, Debug)]
enum Binding {
    Value(Local),
    /// A local function: its formals and body, inlined at each call.
    Function(Sexp, Sexp),
}

#[derive(Default)]
struct Scope {
    names: FxHashMap<Sexp, Binding>,
}

/// An inlined function's exit: the block a `return` jumps to and the local it writes.
struct Inlined {
    exit: BlockId,
    result: Option<Local>,
}

struct Lowering<'c> {
    builder: Builder,
    closure: Sexp,
    env: Sexp,
    params: SmallVec<[Type; 4]>,
    guess: Type,
    callees: &'c mut dyn Callees,
    /// The function's own names, then one scope per inlined function.
    scopes: Vec<Scope>,
    /// The `next` and `break` targets of the loops around the current point.
    loops: Vec<(BlockId, BlockId)>,
    inlined: Vec<Inlined>,
    speculations: Vec<Speculation>,
    nesting: usize,
}

/// An argument of a call, by name or position.
struct Arguments {
    items: Vec<(Option<String>, Sexp)>,
}

impl Arguments {
    unsafe fn of(call: Sexp) -> Arguments {
        Arguments {
            items: rvalues::pairlist_items(call.cdr())
                .into_iter()
                .map(|(tag, value)| {
                    let name = (tag != rvalues::nil()).then(|| rvalues::symbol_name(tag).into_owned());
                    (name, value)
                })
                .collect(),
        }
    }

    fn named(&self, name: &str) -> Option<Sexp> {
        self.items
            .iter()
            .find(|(tag, _)| tag.as_deref() == Some(name))
            .map(|&(_, value)| value)
    }

    fn positional(&self) -> impl Iterator<Item = Sexp> + '_ {
        self.items
            .iter()
            .filter(|(tag, _)| tag.is_none())
            .map(|&(_, value)| value)
    }

    /// The arguments when they are exactly `count` positional ones.
    fn exactly(&self, count: usize) -> Result<SmallVec<[Sexp; 4]>, Refusal> {
        if self.items.len() != count || self.items.iter().any(|(tag, _)| tag.is_some()) {
            return Err(Refusal("a call with unexpected arguments"));
        }
        Ok(self.items.iter().map(|&(_, value)| value).collect())
    }

    /// Argument `name`, or else the positional argument at `position` among the positional ones.
    fn get(&self, name: &str, position: usize) -> Option<Sexp> {
        self.named(name).or_else(|| self.positional().nth(position))
    }

    fn only_names(&self, allowed: &[&str]) -> bool {
        self.items
            .iter()
            .all(|(tag, _)| tag.as_deref().is_none_or(|tag| allowed.contains(&tag)))
    }
}

impl Lowering<'_> {
    fn bind(&mut self, symbol: Sexp, binding: Binding) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.names.insert(symbol, binding);
        }
    }

    fn lookup(&self, symbol: Sexp) -> Option<Binding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.names.get(&symbol).copied())
    }

    /// Lowers `expression`, giving the local holding its value, or `None` when it never
    /// completes (a `return`, `break` or `next`).
    unsafe fn expression(&mut self, expression: Sexp) -> Result<Option<Local>, Refusal> {
        self.nesting += 1;
        if self.nesting > MAX_NESTING {
            return Err(Refusal("expressions nest too deeply"));
        }
        let value = self.expression_inner(expression);
        self.nesting -= 1;
        value
    }

    /// Lowers `expression` that must complete.
    unsafe fn value(&mut self, expression: Sexp) -> Result<Local, Refusal> {
        self.expression(expression)?
            .ok_or(Refusal("an argument that never completes"))
    }

    unsafe fn expression_inner(&mut self, expression: Sexp) -> Result<Option<Local>, Refusal> {
        match expression.kind() {
            SexpType::Integer | SexpType::Real | SexpType::Logical | SexpType::String
            | SexpType::Nil => self.constant(expression).map(Some),
            SexpType::Symbol => self.read(expression).map(Some),
            SexpType::Language => self.call(expression),
            _ => Err(Refusal("an expression of an unsupported kind")),
        }
    }

    unsafe fn constant(&mut self, value: Sexp) -> Result<Local, Refusal> {
        if value.kind() == SexpType::Nil {
            return Ok(self.builder.emit(Type::Null, Op::Const(Constant::Null)));
        }
        if value.attrib() != rvalues::nil() || rvalues::length(value) != 1 {
            return Err(Refusal("a constant that is not a plain scalar"));
        }
        let (ty, constant) = match value.kind() {
            SexpType::Integer => (Type::Int, Constant::Int(rvalues::ints(value)[0])),
            SexpType::Logical => (Type::Lgl, Constant::Lgl(rvalues::ints(value)[0])),
            SexpType::Real => (Type::Dbl, Constant::Dbl(rvalues::reals(value)[0])),
            SexpType::String => {
                let text = rvalues::string_at(value, 0)
                    .ok_or(Refusal("an NA string constant"))?;
                return Ok(self.string(&text));
            }
            _ => return Err(Refusal("a constant of an unsupported type")),
        };
        Ok(self.builder.emit(ty, Op::Const(constant)))
    }

    /// A string constant, built once at compile time and kept for the session.
    unsafe fn string(&mut self, text: &str) -> Local {
        let words = values::ry_str_static(text.as_ptr(), text.len() as u64);
        self.builder
            .emit(Type::Str, Op::Const(Constant::Str(words.lo, words.hi)))
    }

    unsafe fn read(&mut self, symbol: Sexp) -> Result<Local, Refusal> {
        match self.lookup(symbol) {
            Some(Binding::Value(local)) => Ok(local),
            Some(Binding::Function(..)) => Err(Refusal("a local function used as a value")),
            None => {
                let name = rvalues::symbol_name(symbol);
                match &*name {
                    "pi" if self.is_base_value(symbol) => Ok(self
                        .builder
                        .emit(Type::Dbl, Op::Const(Constant::Dbl(std::f64::consts::PI)))),
                    _ => {
                        note(&name);
                        Err(Refusal("a variable that is not a local"))
                    }
                }
            }
        }
    }

    /// Whether `symbol` finds base R's own binding from the closure's environment.
    unsafe fn is_base_value(&self, symbol: Sexp) -> bool {
        let heap = interp::heap();
        let base = heap.known().base_env;
        let mut env = self.env;
        let empty = heap.known().empty_env;
        while env != empty {
            if env == base {
                return true;
            }
            if heap.frame_cell(env, symbol).is_some() {
                return false;
            }
            env = env.enclosure();
        }
        false
    }

    unsafe fn assign(&mut self, symbol: Sexp, value: Local) -> Result<Local, Refusal> {
        let ty = self.builder.ty(value);
        match self.lookup_assignable(symbol) {
            Some(variable) => {
                if self.builder.ty(variable) != ty {
                    return Err(Refusal("a variable that changes type"));
                }
                if variable != value {
                    self.builder.push(Some(variable), Op::Copy(value));
                }
                Ok(variable)
            }
            None => {
                let variable = self.builder.emit(ty, Op::Copy(value));
                self.bind(symbol, Binding::Value(variable));
                Ok(variable)
            }
        }
    }

    /// The variable an assignment in the current scope writes, if it exists there.
    fn lookup_assignable(&self, symbol: Sexp) -> Option<Local> {
        match self.scopes.last()?.names.get(&symbol) {
            Some(Binding::Value(local)) if (local.0 as usize) >= self.params.len() => Some(*local),
            _ => None,
        }
    }

    unsafe fn call(&mut self, call: Sexp) -> Result<Option<Local>, Refusal> {
        let head = call.car();
        if head.kind() != SexpType::Symbol {
            return Err(Refusal("a call of a computed function"));
        }
        let name = rvalues::symbol_name(head).into_owned();
        // The language's own forms, unless the program rebinds them.
        match name.as_str() {
            "{" => return self.sequence(call),
            "(" => {
                let [inner] = self.arguments(call, 1)?[..] else {
                    return Err(Refusal("a malformed parenthesis"));
                };
                return self.expression(inner);
            }
            "<-" | "=" => return self.assignment(call).map(Some),
            "if" => return self.conditional(call),
            "for" => return self.for_loop(call),
            "while" => return self.while_loop(call),
            "repeat" => return self.repeat_loop(call),
            "break" | "next" => {
                let &(continue_to, break_to) =
                    self.loops.last().ok_or(Refusal("`break` outside a loop"))?;
                let target = if name == "break" { break_to } else { continue_to };
                self.builder.terminate(Terminator::Jump(target));
                let unreachable = self.builder.block();
                self.builder.switch_to(unreachable);
                return Ok(None);
            }
            "return" => {
                let arguments = Arguments::of(call);
                let value = match arguments.items.len() {
                    0 => self.builder.emit(Type::Null, Op::Const(Constant::Null)),
                    1 => self.value(arguments.items[0].1)?,
                    _ => return Err(Refusal("`return` with several values")),
                };
                self.exit(value)?;
                return Ok(None);
            }
            "function" => return Err(Refusal("a function literal that escapes")),
            "<<-" => return Err(Refusal("a superassignment")),
            "stop" => {
                // The interpreter raises the error, with its message and call.
                self.builder.terminate(Terminator::Fail);
                let unreachable = self.builder.block();
                self.builder.switch_to(unreachable);
                return Ok(None);
            }
            _ => {}
        }
        if let Some(Binding::Function(formals, body)) = self.lookup(head) {
            let arguments = Arguments::of(call);
            let values = self.call_arguments(&arguments)?;
            return self.inline(formals, body, &values);
        }
        if self.lookup(head).is_some() {
            return Err(Refusal("a call of a local variable"));
        }
        let Some(speculation) = resolve_function(head, self.env) else {
            note(&name);
            return Err(Refusal("a function that does not resolve"));
        };
        let function = speculation.function;
        if is_base_function(&speculation) {
            self.speculations.push(speculation);
            return self.builtin(&name, call).map(Some);
        }
        if function.kind() == SexpType::Closure && interp::annotations::is_typed(function) {
            self.speculations.push(speculation);
            let arguments = Arguments::of(call);
            if arguments.items.iter().any(|(tag, _)| tag.is_some()) {
                return Err(Refusal("a typed call with named arguments"));
            }
            let values = self.call_arguments(&arguments)?;
            let types: SmallVec<[Type; 4]> = values.iter().map(|&value| self.builder.ty(value)).collect();
            let (index, result) = if function == self.closure && types[..] == self.params[..] {
                (crate::lower::SELF, self.guess)
            } else {
                self.callees.callee(function, &types)?
            };
            return Ok(Some(self.builder.emit(result, Op::CallTyped(index, values))));
        }
        note(&name);
        Err(Refusal("a call of a function the typed core does not know"))
    }

    unsafe fn call_arguments(&mut self, arguments: &Arguments) -> Result<SmallVec<[Local; 4]>, Refusal> {
        let mut values = SmallVec::new();
        for &(_, argument) in &arguments.items {
            values.push(self.value(argument)?);
        }
        Ok(values)
    }

    unsafe fn arguments(&self, call: Sexp, count: usize) -> Result<SmallVec<[Sexp; 4]>, Refusal> {
        Arguments::of(call).exactly(count)
    }

    /// Leaves the innermost inlined function, or the function itself, with `value`.
    unsafe fn exit(&mut self, value: Local) -> Result<(), Refusal> {
        match self.inlined.last() {
            Some(inlined) => {
                let exit = inlined.exit;
                match inlined.result {
                    Some(result) => {
                        if self.builder.ty(result) != self.builder.ty(value) {
                            return Err(Refusal("a `return` of another type"));
                        }
                        self.builder.push(Some(result), Op::Copy(value));
                    }
                    None => return Err(Refusal("a `return` before the result type is known")),
                }
                self.builder.terminate(Terminator::Jump(exit));
            }
            None => {
                if self.builder.ty(value) != self.guess && self.guess != Type::Null {
                    return Err(Refusal("a `return` of another type"));
                }
                self.builder.terminate(Terminator::Return(value));
            }
        }
        let unreachable = self.builder.block();
        self.builder.switch_to(unreachable);
        Ok(())
    }

    unsafe fn sequence(&mut self, call: Sexp) -> Result<Option<Local>, Refusal> {
        let items = rvalues::pairlist_items(call.cdr());
        let mut last = None;
        if items.is_empty() {
            return Ok(Some(self.builder.emit(Type::Null, Op::Const(Constant::Null))));
        }
        for (_, item) in items {
            last = self.expression(item)?;
            if last.is_none() {
                return Ok(None);
            }
        }
        Ok(last)
    }

    unsafe fn assignment(&mut self, call: Sexp) -> Result<Local, Refusal> {
        let [target, value] = self.arguments(call, 2)?[..] else {
            return Err(Refusal("a malformed assignment"));
        };
        if target.kind() == SexpType::Symbol {
            if value.kind() == SexpType::Language
                && value.car().kind() == SexpType::Symbol
                && rvalues::symbol_name(value.car()) == "function"
                && self.lookup(value.car()).is_none()
            {
                let parts = rvalues::pairlist_items(value.cdr());
                let (Some(&(_, formals)), Some(&(_, body))) = (parts.first(), parts.get(1)) else {
                    return Err(Refusal("a malformed function literal"));
                };
                self.bind(target, Binding::Function(formals, body));
                return Ok(self.builder.emit(Type::Null, Op::Const(Constant::Null)));
            }
            let value = self.value(value)?;
            return self.assign(target, value);
        }
        // `x[[i]] <- value` and `x[i] <- value` with a scalar index.
        if target.kind() == SexpType::Language && target.car().kind() == SexpType::Symbol {
            let head = rvalues::symbol_name(target.car());
            if head == "[[" || head == "[" {
                let [vector, index] = self.arguments(target, 2)?[..] else {
                    return Err(Refusal("an indexed assignment of an unsupported form"));
                };
                if vector.kind() != SexpType::Symbol {
                    return Err(Refusal("an indexed assignment into an expression"));
                }
                let variable = self
                    .lookup_assignable(vector)
                    .ok_or(Refusal("an indexed assignment into a parameter or a non-local"))?;
                let index = self.value(index)?;
                let index = self.integer_index(index)?;
                let value = self.value(value)?;
                let items = self
                    .builder
                    .ty(variable)
                    .items()
                    .ok_or(Refusal("an indexed assignment into a non-vector"))?;
                let value = self.convert_scalar(value, items)?;
                let updated = self
                    .builder
                    .emit(self.builder.ty(variable), Op::Set(variable, index, value));
                self.builder.push(Some(variable), Op::Copy(updated));
                return Ok(value);
            }
        }
        Err(Refusal("an assignment of an unsupported form"))
    }

    unsafe fn integer_index(&mut self, index: Local) -> Result<Local, Refusal> {
        match self.builder.ty(index) {
            Type::Int => Ok(index),
            Type::Dbl => Err(Refusal("a double index")),
            _ => Err(Refusal("an index that is not an integer")),
        }
    }

    /// `value` as a scalar of `item`, converting a narrower number as R would on assignment.
    unsafe fn convert_scalar(&mut self, value: Local, item: Item) -> Result<Local, Refusal> {
        let ty = self.builder.ty(value);
        match (ty, item) {
            (Type::Int, Item::Int) | (Type::Dbl, Item::Dbl) | (Type::Lgl, Item::Lgl)
            | (Type::Str, Item::Str) => Ok(value),
            (Type::Int | Type::Lgl, Item::Dbl) => Ok(self.builder.emit(Type::Dbl, Op::ToDbl(value))),
            (Type::Lgl, Item::Int) => Ok(self.builder.emit(Type::Int, Op::LglToInt(value))),
            _ => Err(Refusal("an item of another type")),
        }
    }

    unsafe fn condition(&mut self, condition: Sexp) -> Result<Local, Refusal> {
        let value = self.value(condition)?;
        match self.builder.ty(value) {
            Type::Lgl => Ok(self.builder.emit(Type::Bool, Op::Truth(value))),
            Type::Bool => Ok(value),
            _ => Err(Refusal("a condition that is not a logical")),
        }
    }

    unsafe fn conditional(&mut self, call: Sexp) -> Result<Option<Local>, Refusal> {
        let items = rvalues::pairlist_items(call.cdr());
        let (condition, then, otherwise) = match items[..] {
            [(_, condition), (_, then)] => (condition, then, None),
            [(_, condition), (_, then), (_, otherwise)] => (condition, then, Some(otherwise)),
            _ => return Err(Refusal("a malformed `if`")),
        };
        let condition = self.condition(condition)?;
        let then_block = self.builder.block();
        let else_block = self.builder.block();
        let join = self.builder.block();
        self.builder.terminate(Terminator::Branch {
            condition,
            then: then_block,
            otherwise: else_block,
        });
        self.builder.switch_to(then_block);
        let then_value = self.expression(then)?;
        let then_end = self.builder.current;
        self.builder.switch_to(else_block);
        let else_value = match otherwise {
            Some(otherwise) => self.expression(otherwise)?,
            None => Some(self.builder.emit(Type::Null, Op::Const(Constant::Null))),
        };
        let else_end = self.builder.current;
        let result = match (then_value, else_value) {
            (None, None) => None,
            (Some(value), None) | (None, Some(value)) => {
                let result = self.builder.local(self.builder.ty(value));
                let end = if then_value.is_some() { then_end } else { else_end };
                self.builder.switch_to(end);
                self.builder.push(Some(result), Op::Copy(value));
                Some(result)
            }
            (Some(a), Some(b)) => {
                if self.builder.ty(a) == self.builder.ty(b) {
                    let result = self.builder.local(self.builder.ty(a));
                    self.builder.switch_to(then_end);
                    self.builder.push(Some(result), Op::Copy(a));
                    self.builder.switch_to(else_end);
                    self.builder.push(Some(result), Op::Copy(b));
                    Some(result)
                } else {
                    // The branches disagree, which is fine where nothing reads the value.
                    let result = self.builder.local(Type::Null);
                    for end in [then_end, else_end] {
                        self.builder.switch_to(end);
                        self.builder.push(Some(result), Op::Const(Constant::Null));
                    }
                    Some(result)
                }
            }
        };
        if then_value.is_some() {
            self.builder.switch_to(then_end);
            self.builder.terminate(Terminator::Jump(join));
        }
        if else_value.is_some() {
            self.builder.switch_to(else_end);
            self.builder.terminate(Terminator::Jump(join));
        }
        self.builder.switch_to(join);
        if result.is_none() {
            self.builder.terminate(Terminator::Fail);
        }
        Ok(result)
    }

    /// The loop header that checks for an interrupt and the condition block after it.
    fn loop_header(&mut self) -> BlockId {
        let header = self.builder.block();
        self.builder.terminate(Terminator::Jump(header));
        self.builder.switch_to(header);
        self.builder.push(None, Op::Poll);
        header
    }

    unsafe fn for_loop(&mut self, call: Sexp) -> Result<Option<Local>, Refusal> {
        let [variable, sequence, body] = self.arguments(call, 3)?[..] else {
            return Err(Refusal("a malformed `for`"));
        };
        // A counted loop over `seq_len(n)`, `seq_along(x)` or `a:b` allocates nothing.
        let (start, end, step, items) = match self.counted_range(sequence)? {
            Some((start, end, step)) => (start, end, step, None),
            None => {
                let vector = self.value(sequence)?;
                let ty = self.builder.ty(vector);
                ty.items().ok_or(Refusal("a `for` over something that is not a vector"))?;
                // The loop reads the value the sequence had when it started.
                let held = self.builder.emit(ty, Op::Copy(vector));
                let one = self.int(1);
                let length = self.builder.emit(Type::Int, Op::Length(held));
                let step = self.int(1);
                (one, length, Some(step), Some(held))
            }
        };
        let counter = self.builder.emit(Type::Int, Op::Copy(start));
        let exit = self.builder.block();
        let step = match step {
            // `seq_len`, `seq_along` and a vector count up from 1 to an end that is not `NA` and
            // not negative, and run no iteration when it is 0.
            Some(step) => {
                let zero = self.int(0);
                let empty = self.builder.emit(Type::Bool, Op::Same(end, zero));
                let go = self.builder.block();
                self.builder.terminate(Terminator::Branch { condition: empty, then: exit, otherwise: go });
                self.builder.switch_to(go);
                step
            }
            // `a:b` always holds at least one item and counts down when `b < a`. The comparison
            // fails on an `NA` end, as `:` does.
            None => {
                let down = self.builder.emit(Type::Lgl, Op::Compare(Compare::Lt, Item::Int, end, start));
                let down = self.builder.emit(Type::Bool, Op::Truth(down));
                let minus = self.int(-1);
                let plus = self.int(1);
                let step = self.builder.local(Type::Int);
                let (a, b, join) = (self.builder.block(), self.builder.block(), self.builder.block());
                self.builder.terminate(Terminator::Branch { condition: down, then: a, otherwise: b });
                self.builder.switch_to(a);
                self.builder.push(Some(step), Op::Copy(minus));
                self.builder.terminate(Terminator::Jump(join));
                self.builder.switch_to(b);
                self.builder.push(Some(step), Op::Copy(plus));
                self.builder.terminate(Terminator::Jump(join));
                self.builder.switch_to(join);
                step
            }
        };
        // The latch leaves once the counter has reached the end, so stepping never passes it.
        let header = self.loop_header();
        let item = match items {
            Some(held) => self.builder.emit(
                Type::of_item(self.builder.ty(held).items().ok_or(Refusal("a loop over a non-vector"))?),
                Op::Get(held, counter),
            ),
            None => counter,
        };
        self.assign(variable, item)?;
        let latch = self.builder.block();
        self.loops.push((latch, exit));
        let completed = self.expression(body)?;
        self.loops.pop();
        if completed.is_some() {
            self.builder.terminate(Terminator::Jump(latch));
        }
        self.builder.switch_to(latch);
        let done = self.builder.emit(Type::Bool, Op::Same(counter, end));
        let next_block = self.builder.block();
        self.builder.terminate(Terminator::Branch { condition: done, then: exit, otherwise: next_block });
        self.builder.switch_to(next_block);
        let next = self.builder.emit(Type::Int, Op::Step(counter, step));
        self.builder.push(Some(counter), Op::Copy(next));
        self.builder.terminate(Terminator::Jump(header));
        self.builder.switch_to(exit);
        Ok(Some(self.builder.emit(Type::Null, Op::Const(Constant::Null))))
    }

    /// The start, end and step (`None` for `a:b`, whose direction depends on its ends) of a
    /// counted sequence.
    unsafe fn counted_range(
        &mut self,
        sequence: Sexp,
    ) -> Result<Option<(Local, Local, Option<Local>)>, Refusal> {
        if sequence.kind() != SexpType::Language || sequence.car().kind() != SexpType::Symbol {
            return Ok(None);
        }
        let head = sequence.car();
        if self.lookup(head).is_some() {
            return Ok(None);
        }
        let name = rvalues::symbol_name(head).into_owned();
        if !matches!(name.as_str(), "seq_len" | "seq_along" | ":") {
            return Ok(None);
        }
        let Some(speculation) = resolve_function(head, self.env) else {
            return Ok(None);
        };
        if !is_base_function(&speculation) {
            return Ok(None);
        }
        self.speculations.push(speculation);
        match name.as_str() {
            "seq_len" | "seq_along" => {
                let [argument] = self.arguments(sequence, 1)?[..] else {
                    return Err(Refusal("a malformed sequence"));
                };
                let value = self.value(argument)?;
                let end = if name == "seq_len" {
                    let count = self.integer_count(value)?;
                    let zero = self.int(0);
                    let negative = self.builder.emit(Type::Lgl, Op::Compare(Compare::Lt, Item::Int, count, zero));
                    self.fail_if(negative);
                    count
                } else {
                    self.length(value)
                };
                let one = self.int(1);
                let step = self.int(1);
                Ok(Some((one, end, Some(step))))
            }
            _ => {
                let [from, to] = self.arguments(sequence, 2)?[..] else {
                    return Err(Refusal("a malformed `:`"));
                };
                let from = self.value(from)?;
                let to = self.value(to)?;
                let from = self.range_end(from)?;
                let to = self.range_end(to)?;
                Ok(Some((from, to, None)))
            }
        }
    }

    /// Fails the call when `condition`, a logical, is true or `NA`.
    fn fail_if(&mut self, condition: Local) {
        let truth = self.builder.emit(Type::Bool, Op::Truth(condition));
        let (fail, go) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch { condition: truth, then: fail, otherwise: go });
        self.builder.switch_to(fail);
        self.builder.terminate(Terminator::Fail);
        self.builder.switch_to(go);
    }

    /// An end of `a:b` as an integer: integers as they are, and a double only when it is a
    /// whole number constant, which R also turns into an integer sequence.
    unsafe fn range_end(&mut self, value: Local) -> Result<Local, Refusal> {
        match self.builder.ty(value) {
            Type::Int => Ok(value),
            Type::Dbl => {
                let constant = self.constant_double(value).ok_or(Refusal("a `:` of computed doubles"))?;
                if constant.fract() != 0.0 || constant.abs() > 1e9 {
                    return Err(Refusal("a `:` of fractional doubles"));
                }
                Ok(self.int(constant as i32))
            }
            _ => Err(Refusal("a `:` of something other than numbers")),
        }
    }

    /// The value of a double local defined by a constant in the current block.
    fn constant_double(&self, local: Local) -> Option<f64> {
        let block = &self.builder.function.blocks[self.builder.current.0 as usize];
        block.insts.iter().rev().find_map(|inst| match inst.op {
            Op::Const(Constant::Dbl(value)) if inst.dest == Some(local) => Some(value),
            _ => None,
        })
    }

    /// A count argument (`seq_len(n)`, `numeric(n)`) as an integer.
    unsafe fn integer_count(&mut self, value: Local) -> Result<Local, Refusal> {
        match self.builder.ty(value) {
            Type::Int => Ok(value),
            Type::Dbl => {
                let constant = self.constant_double(value).ok_or(Refusal("a count that is a computed double"))?;
                if constant.fract() != 0.0 || !(0.0..=2e9).contains(&constant) {
                    return Err(Refusal("a count that is not a whole number"));
                }
                Ok(self.int(constant as i32))
            }
            _ => Err(Refusal("a count that is not a number")),
        }
    }

    unsafe fn while_loop(&mut self, call: Sexp) -> Result<Option<Local>, Refusal> {
        let [condition, body] = self.arguments(call, 2)?[..] else {
            return Err(Refusal("a malformed `while`"));
        };
        let header = self.loop_header();
        let condition = self.condition(condition)?;
        let (body_block, exit) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch { condition, then: body_block, otherwise: exit });
        self.builder.switch_to(body_block);
        self.loops.push((header, exit));
        let completed = self.expression(body)?;
        self.loops.pop();
        if completed.is_some() {
            self.builder.terminate(Terminator::Jump(header));
        }
        self.builder.switch_to(exit);
        Ok(Some(self.builder.emit(Type::Null, Op::Const(Constant::Null))))
    }

    unsafe fn repeat_loop(&mut self, call: Sexp) -> Result<Option<Local>, Refusal> {
        let [body] = self.arguments(call, 1)?[..] else {
            return Err(Refusal("a malformed `repeat`"));
        };
        let header = self.loop_header();
        let exit = self.builder.block();
        self.loops.push((header, exit));
        let completed = self.expression(body)?;
        self.loops.pop();
        if completed.is_some() {
            self.builder.terminate(Terminator::Jump(header));
        }
        self.builder.switch_to(exit);
        Ok(Some(self.builder.emit(Type::Null, Op::Const(Constant::Null))))
    }

    /// Inlines a local function or function literal called with `arguments`.
    unsafe fn inline(
        &mut self,
        formals: Sexp,
        body: Sexp,
        arguments: &[Local],
    ) -> Result<Option<Local>, Refusal> {
        if self.inlined.len() >= MAX_INLINE_DEPTH {
            return Err(Refusal("local functions inline too deeply"));
        }
        let formals = rvalues::pairlist_items(formals);
        if formals.len() != arguments.len() {
            return Err(Refusal("a local function called with another number of arguments"));
        }
        let mut scope = Scope::default();
        for (&(symbol, _), &argument) in formals.iter().zip(arguments) {
            if symbol == rvalues::symbols().dots {
                return Err(Refusal("a local function with `...`"));
            }
            // A parameter is a fresh variable, so assigning it never touches the caller's.
            let ty = self.builder.ty(argument);
            let variable = self.builder.local(ty);
            self.builder.push(Some(variable), Op::Copy(argument));
            scope.names.insert(symbol, Binding::Value(variable));
        }
        self.scopes.push(scope);
        let exit = self.builder.block();
        // The result's type is the body's, which a `return` before the end must match: the
        // body is lowered first into its own blocks with the result local still open.
        self.inlined.push(Inlined { exit, result: None });
        let has_return = contains_return(body);
        let value = if has_return {
            Err(Refusal("a `return` inside a local function"))
        } else {
            self.expression(body)
        };
        self.inlined.pop();
        self.scopes.pop();
        let value = value?;
        match value {
            Some(value) => {
                let result = self.builder.local(self.builder.ty(value));
                self.builder.push(Some(result), Op::Copy(value));
                self.builder.terminate(Terminator::Jump(exit));
                self.builder.switch_to(exit);
                Ok(Some(result))
            }
            None => {
                self.builder.switch_to(exit);
                self.builder.terminate(Terminator::Fail);
                Ok(None)
            }
        }
    }

    fn int(&mut self, value: i32) -> Local {
        self.builder.emit(Type::Int, Op::Const(Constant::Int(value)))
    }

    fn length(&mut self, value: Local) -> Local {
        if self.builder.ty(value).items().is_some() {
            self.builder.emit(Type::Int, Op::Length(value))
        } else {
            self.int(1)
        }
    }

    /// Lowers a call of base R's or the standard library's function `name`.
    unsafe fn builtin(&mut self, name: &str, call: Sexp) -> Result<Local, Refusal> {
        let arguments = Arguments::of(call);
        match name {
            "+" | "-" | "*" | "/" | "^" | "%%" | "%/%" => {
                let operands: SmallVec<[Sexp; 4]> = arguments.positional().collect();
                if arguments.items.len() != operands.len() {
                    return Err(Refusal("arithmetic with named arguments"));
                }
                match operands[..] {
                    [only] if name == "-" => {
                        let value = self.value(only)?;
                        self.math(Math::Neg, value)
                    }
                    [only] if name == "+" => {
                        let value = self.value(only)?;
                        if matches!(self.builder.ty(value), Type::Int | Type::Dbl) {
                            Ok(value)
                        } else {
                            Err(Refusal("a unary `+` of a non-number"))
                        }
                    }
                    [a, b] => {
                        let a = self.value(a)?;
                        let b = self.value(b)?;
                        let op = match name {
                            "+" => Arith::Add,
                            "-" => Arith::Sub,
                            "*" => Arith::Mul,
                            "/" => Arith::Div,
                            "^" => Arith::Pow,
                            "%%" => Arith::Mod,
                            _ => Arith::IntDiv,
                        };
                        self.arith(op, a, b)
                    }
                    _ => Err(Refusal("arithmetic with an unexpected number of operands")),
                }
            }
            "==" | "!=" | "<" | "<=" | ">" | ">=" => {
                let [a, b] = arguments.exactly(2)?[..] else {
                    return Err(Refusal("a malformed comparison"));
                };
                let a = self.value(a)?;
                let b = self.value(b)?;
                let op = match name {
                    "==" => Compare::Eq,
                    "!=" => Compare::Ne,
                    "<" => Compare::Lt,
                    "<=" => Compare::Le,
                    ">" => Compare::Gt,
                    _ => Compare::Ge,
                };
                self.compare(op, a, b)
            }
            "!" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a malformed `!`"));
                };
                let a = self.value(a)?;
                match self.builder.ty(a) {
                    Type::Lgl | Type::Bool => Ok(self.builder.emit(Type::Lgl, Op::Not(a))),
                    Type::Vector { item: Item::Lgl, .. } => {
                        Ok(self.builder.emit(Type::vector(Item::Lgl), Op::VectorNot(a)))
                    }
                    _ => Err(Refusal("a `!` of something other than logicals")),
                }
            }
            "&" | "|" => {
                let [a, b] = arguments.exactly(2)?[..] else {
                    return Err(Refusal("a malformed logical operator"));
                };
                let a = self.value(a)?;
                let b = self.value(b)?;
                let and = name == "&";
                match (self.builder.ty(a), self.builder.ty(b)) {
                    (Type::Lgl | Type::Bool, Type::Lgl | Type::Bool) => Ok(self
                        .builder
                        .emit(Type::Lgl, if and { Op::And(a, b) } else { Op::Or(a, b) })),
                    (Type::Vector { item: Item::Lgl, .. } | Type::Lgl, Type::Vector { item: Item::Lgl, .. } | Type::Lgl) => {
                        let a = self.as_vector(a)?;
                        let b = self.as_vector(b)?;
                        Ok(self.builder.emit(Type::vector(Item::Lgl), Op::VectorLogic(and, a, b)))
                    }
                    _ => Err(Refusal("a logical operator on something other than logicals")),
                }
            }
            "&&" | "||" => {
                let [a, b] = arguments.exactly(2)?[..] else {
                    return Err(Refusal("a malformed `&&`"));
                };
                let a = self.condition(a)?;
                let result = self.builder.local(Type::Lgl);
                let (evaluate, short, join) = (self.builder.block(), self.builder.block(), self.builder.block());
                let (then, otherwise) = if name == "&&" { (evaluate, short) } else { (short, evaluate) };
                self.builder.terminate(Terminator::Branch { condition: a, then, otherwise });
                self.builder.switch_to(short);
                let constant = self.builder.emit(
                    Type::Lgl,
                    Op::Const(Constant::Lgl(i32::from(name == "||"))),
                );
                self.builder.push(Some(result), Op::Copy(constant));
                self.builder.terminate(Terminator::Jump(join));
                self.builder.switch_to(evaluate);
                let b = self.condition(b)?;
                let b = self.builder.emit(Type::Lgl, Op::Copy(b));
                self.builder.function.locals[b.0 as usize] = Type::Lgl;
                self.builder.push(Some(result), Op::Copy(b));
                self.builder.terminate(Terminator::Jump(join));
                self.builder.switch_to(join);
                Ok(result)
            }
            "length" | "List.len" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a malformed `length`"));
                };
                let a = self.value(a)?;
                if matches!(self.builder.ty(a), Type::Null) {
                    return Ok(self.int(0));
                }
                Ok(self.length(a))
            }
            "seq_len" => {
                let [n] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a malformed `seq_len`"));
                };
                let n = self.value(n)?;
                let n = self.integer_count(n)?;
                Ok(self.builder.emit(Type::vector(Item::Int), Op::SeqLen(n)))
            }
            "seq_along" => {
                let [x] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a malformed `seq_along`"));
                };
                let x = self.value(x)?;
                let n = self.length(x);
                Ok(self.builder.emit(Type::vector(Item::Int), Op::SeqLen(n)))
            }
            ":" => {
                let [from, to] = arguments.exactly(2)?[..] else {
                    return Err(Refusal("a malformed `:`"));
                };
                let from = self.value(from)?;
                let to = self.value(to)?;
                let from = self.range_end(from)?;
                let to = self.range_end(to)?;
                Ok(self.builder.emit(Type::vector(Item::Int), Op::Range(from, to)))
            }
            "rep" | "rep.int" | "List.repeat" => {
                if !arguments.only_names(&["x", "times", "value", "count"]) || arguments.items.len() != 2 {
                    return Err(Refusal("a `rep` of an unsupported form"));
                }
                let value = arguments
                    .get("x", 0)
                    .or_else(|| arguments.named("value"))
                    .ok_or(Refusal("a `rep` without a value"))?;
                let times = arguments
                    .get("times", 1)
                    .or_else(|| arguments.named("count"))
                    .or_else(|| arguments.positional().nth(1))
                    .ok_or(Refusal("a `rep` without a count"))?;
                let value = self.value(value)?;
                let times = self.value(times)?;
                let times = self.integer_count(times)?;
                let item = self
                    .builder
                    .ty(value)
                    .item()
                    .ok_or(Refusal("a `rep` of a vector"))?;
                let value = self.convert_scalar(value, item)?;
                let ty = if name == "List.repeat" { Type::List(item) } else { Type::vector(item) };
                Ok(self.builder.emit(ty, Op::Repeat(value, times)))
            }
            "as.Array" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("an `as.Array` of an unsupported form"));
                };
                // An Array holds no NA. A fresh vector from a constructor holds zeros, empty
                // strings or `FALSE`, so it converts without the check the interpreter runs.
                let fresh = a.kind() == SexpType::Language
                    && a.car().kind() == SexpType::Symbol
                    && matches!(
                        &*rvalues::symbol_name(a.car()),
                        "numeric" | "double" | "integer" | "logical" | "character"
                    );
                let a = self.value(a)?;
                match self.builder.ty(a) {
                    Type::Vector { array: true, .. } => Ok(a),
                    Type::Vector { item, array: false } if fresh => {
                        let ty = Type::Vector { item, array: true };
                        Ok(self.builder.emit(ty, Op::Relabel(a, ty)))
                    }
                    _ => Err(Refusal("an `as.Array` of a vector that may hold NA")),
                }
            }
            "numeric" | "double" | "integer" | "logical" | "character" => {
                let item = match name {
                    "numeric" | "double" => Item::Dbl,
                    "integer" => Item::Int,
                    "logical" => Item::Lgl,
                    _ => Item::Str,
                };
                let count = match arguments.items.len() {
                    0 => self.int(0),
                    1 if arguments.only_names(&["length"]) => {
                        let count = self.value(arguments.items[0].1)?;
                        self.integer_count(count)?
                    }
                    _ => return Err(Refusal("a vector constructor of an unsupported form")),
                };
                Ok(self.builder.emit(Type::vector(item), Op::NewVector(item, count)))
            }
            "sum" | "prod" | "mean" | "List.sum" | "List.product" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a reduction of several arguments"));
                };
                let a = self.value(a)?;
                let reduce = match name {
                    "sum" | "List.sum" => Reduce::Sum,
                    "prod" | "List.product" => Reduce::Prod,
                    _ => Reduce::Mean,
                };
                self.reduce(reduce, a)
            }
            "max" | "min" => {
                let operands = arguments.exactly(arguments.items.len())?;
                let reduce = if name == "max" { Reduce::Max } else { Reduce::Min };
                match operands[..] {
                    [a] => {
                        let a = self.value(a)?;
                        self.reduce(reduce, a)
                    }
                    [a, b] => {
                        let a = self.value(a)?;
                        let b = self.value(b)?;
                        if self.builder.ty(a).items().is_some() || self.builder.ty(b).items().is_some() {
                            return Err(Refusal("a `max` of vectors"));
                        }
                        let op = if name == "max" { Arith::Max } else { Arith::Min };
                        self.arith(op, a, b)
                    }
                    _ => Err(Refusal("a `max` of an unsupported form")),
                }
            }
            "any" | "all" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a malformed `any`"));
                };
                let a = self.value(a)?;
                let reduce = if name == "any" { Reduce::Any } else { Reduce::All };
                match self.builder.ty(a) {
                    Type::Lgl => Ok(a),
                    Type::Vector { item: Item::Lgl, .. } => {
                        Ok(self.builder.emit(Type::Lgl, Op::Reduce(reduce, Item::Lgl, a)))
                    }
                    _ => Err(Refusal("an `any` of something other than logicals")),
                }
            }
            "abs" | "sqrt" | "exp" | "log" | "floor" | "ceiling" | "trunc" | "sin" | "cos"
            | "tan" | "log2" | "log10" | "sign" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a math function of several arguments"));
                };
                let a = self.value(a)?;
                let op = match name {
                    "abs" => Math::Abs,
                    "sqrt" => Math::Sqrt,
                    "exp" => Math::Exp,
                    "log" => Math::Log,
                    "floor" => Math::Floor,
                    "ceiling" => Math::Ceiling,
                    "trunc" => Math::Trunc,
                    "sin" => Math::Sin,
                    "cos" => Math::Cos,
                    "tan" => Math::Tan,
                    "log2" => Math::Log2,
                    "log10" => Math::Log10,
                    _ => Math::Sign,
                };
                self.math(op, a)
            }
            "round" => {
                let value = arguments.get("x", 0).ok_or(Refusal("a `round` without a value"))?;
                let value = self.value(value)?;
                let value = self.as_double(value)?;
                match arguments.get("digits", 1) {
                    None if arguments.items.len() == 1 => self.math(Math::Round, value),
                    Some(digits) if arguments.items.len() == 2 => {
                        let digits = self.value(digits)?;
                        let digits = match self.builder.ty(digits) {
                            Type::Int => self.builder.emit(Type::Dbl, Op::ToDbl(digits)),
                            Type::Dbl => digits,
                            _ => return Err(Refusal("a `round` to digits that are not a number")),
                        };
                        if self.builder.ty(value).items().is_some() {
                            Ok(self.builder.emit(self.builder.ty(value), Op::VectorRound(value, digits)))
                        } else {
                            Ok(self.builder.emit(Type::Dbl, Op::RoundDigits(value, digits)))
                        }
                    }
                    _ => Err(Refusal("a `round` of an unsupported form")),
                }
            }
            "as.double" | "as.numeric" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a malformed `as.double`"));
                };
                let a = self.value(a)?;
                self.as_double(a)
            }
            "as.character" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a malformed `as.character`"));
                };
                let a = self.value(a)?;
                self.as_string(a)
            }
            "paste" | "paste0" => self.paste(name == "paste0", &arguments),
            "formatC" => {
                if !arguments.only_names(&["x", "width", "flag"]) {
                    return Err(Refusal("a `formatC` of an unsupported form"));
                }
                let value = arguments.get("x", 0).ok_or(Refusal("a `formatC` without a value"))?;
                let value = self.value(value)?;
                if self.builder.ty(value) != Type::Int {
                    return Err(Refusal("a `formatC` of something other than an integer"));
                }
                let width = match arguments.named("width") {
                    Some(width) => constant_integer(width).ok_or(Refusal("a computed `formatC` width"))?,
                    None => 0,
                };
                let zero = match arguments.named("flag") {
                    Some(flag) => match constant_string(flag).as_deref() {
                        Some("0") => true,
                        Some("") => false,
                        _ => return Err(Refusal("a `formatC` flag other than \"0\"")),
                    },
                    None => false,
                };
                Ok(self.builder.emit(Type::Str, Op::FormatPadded(value, width, zero)))
            }
            "sprintf" => {
                let [format, value] = arguments.exactly(2)?[..] else {
                    return Err(Refusal("a `sprintf` of an unsupported form"));
                };
                let format = constant_string(format).ok_or(Refusal("a computed `sprintf` format"))?;
                let value = self.value(value)?;
                let ty = self.builder.ty(value);
                match (format.as_str(), ty) {
                    ("%d", Type::Int) | ("%s", Type::Int | Type::Dbl | Type::Str) => self.as_string(value),
                    (format, Type::Dbl | Type::Int) => {
                        let decimals = format
                            .strip_prefix("%.")
                            .and_then(|rest| rest.strip_suffix('f'))
                            .and_then(|digits| digits.parse::<i32>().ok())
                            .filter(|&digits| (0..=15).contains(&digits))
                            .ok_or(Refusal("a `sprintf` format the typed core does not know"))?;
                        let value = self.as_double(value)?;
                        Ok(self.builder.emit(Type::Str, Op::FormatFixed(value, decimals)))
                    }
                    _ => Err(Refusal("a `sprintf` format the typed core does not know")),
                }
            }
            "nchar" | "toupper" | "tolower" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a string function of an unsupported form"));
                };
                let a = self.value(a)?;
                let text = match name {
                    "nchar" => Text::Nchar,
                    "toupper" => Text::Upper,
                    _ => Text::Lower,
                };
                let result_item = if text == Text::Nchar { Item::Int } else { Item::Str };
                match self.builder.ty(a) {
                    Type::Str => Ok(self.builder.emit(Type::of_item(result_item), Op::Text(text, a))),
                    Type::Vector { item: Item::Str, .. } => {
                        Ok(self.builder.emit(Type::vector(result_item), Op::VectorText(text, a)))
                    }
                    _ => Err(Refusal("a string function of something other than strings")),
                }
            }
            "startsWith" | "endsWith" => {
                let [a, affix] = arguments.exactly(2)?[..] else {
                    return Err(Refusal("a malformed `startsWith`"));
                };
                let a = self.value(a)?;
                let affix = self.value(affix)?;
                if self.builder.ty(affix) != Type::Str {
                    return Err(Refusal("a `startsWith` of a computed prefix vector"));
                }
                let starts = name == "startsWith";
                match self.builder.ty(a) {
                    Type::Str => Ok(self.builder.emit(Type::Lgl, Op::Affix(starts, a, affix))),
                    Type::Vector { item: Item::Str, .. } => {
                        Ok(self.builder.emit(Type::vector(Item::Lgl), Op::VectorAffix(starts, a, affix)))
                    }
                    _ => Err(Refusal("a `startsWith` of something other than strings")),
                }
            }
            "[[" | "[" => {
                let [vector, index] = arguments.exactly(2)?[..] else {
                    return Err(Refusal("an index of an unsupported form"));
                };
                let vector = self.value(vector)?;
                let index = self.value(index)?;
                let index = self.integer_index(index)?;
                let ty = self.builder.ty(vector);
                let item = ty.items().ok_or(Refusal("an index into something other than a vector"))?;
                if name == "[" && matches!(ty, Type::List(_)) {
                    return Err(Refusal("a `[` of a list"));
                }
                Ok(self.builder.emit(Type::of_item(item), Op::Get(vector, index)))
            }
            "lapply" | "vapply" | "List.map" => self.map(name, &arguments),
            "unlist" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("an `unlist` of an unsupported form"));
                };
                let a = self.value(a)?;
                match self.builder.ty(a) {
                    Type::List(item) => Ok(self.builder.emit(Type::vector(item), Op::Relabel(a, Type::vector(item)))),
                    Type::Vector { .. } => Ok(a),
                    _ => Err(Refusal("an `unlist` of something other than a list of scalars")),
                }
            }
            _ => {
                note(name);
                Err(Refusal("a base function the typed core does not know"))
            }
        }
    }

    /// `lapply(x, f)`, `List.map(x, f)` and `vapply(x, f, value)` with `f` a function literal or
    /// a local function, inlined into a loop over the items.
    unsafe fn map(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let (vector, function, template) = match (name, &arguments.exactly(arguments.items.len())?[..]) {
            ("vapply", &[vector, function, template]) => (vector, function, Some(template)),
            ("lapply" | "List.map", &[vector, function]) => (vector, function, None),
            _ => return Err(Refusal("a map of an unsupported form")),
        };
        let (formals, body) = self.function_argument(function)?;
        let vector = self.value(vector)?;
        let ty = self.builder.ty(vector);
        let item = ty.items().ok_or(Refusal("a map over something that is not a vector"))?;
        let declared = match template {
            Some(template) => Some(template_item(template).ok_or(Refusal("a `vapply` template the typed core does not know"))?),
            None => None,
        };
        let held = self.builder.emit(ty, Op::Copy(vector));
        let count = self.builder.emit(Type::Int, Op::Length(held));
        let preheader = self.builder.current;
        // The result's type is known only once the body is lowered, so the output vector is
        // allocated in the preheader afterwards.
        let output = self.builder.local(Type::Null);
        let counter = self.builder.local(Type::Int);
        let one = self.int(1);
        self.builder.push(Some(counter), Op::Copy(one));
        let header = self.loop_header();
        let past = self.builder.emit(Type::Lgl, Op::Compare(Compare::Gt, Item::Int, counter, count));
        let past = self.builder.emit(Type::Bool, Op::Truth(past));
        let (body_block, exit) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch { condition: past, then: exit, otherwise: body_block });
        self.builder.switch_to(body_block);
        let element = self.builder.emit(Type::of_item(item), Op::Get(held, counter));
        let value = self
            .inline(formals, body, &[element])?
            .ok_or(Refusal("a mapped function that never completes"))?;
        let value_item = self
            .builder
            .ty(value)
            .item()
            .ok_or(Refusal("a mapped function whose value is not a scalar"))?;
        let result_item = declared.unwrap_or(value_item);
        let value = self.convert_scalar(value, result_item)?;
        let output_ty = if name == "vapply" { Type::vector(result_item) } else { Type::List(result_item) };
        self.builder.function.locals[output.0 as usize] = output_ty;
        let updated = self.builder.emit(output_ty, Op::Set(output, counter, value));
        self.builder.push(Some(output), Op::Copy(updated));
        let next = self.builder.emit(Type::Int, Op::Arith(Arith::Add, Item::Int, counter, one));
        self.builder.push(Some(counter), Op::Copy(next));
        self.builder.terminate(Terminator::Jump(header));
        // The output is allocated before the loop, now that its type is known.
        self.builder.switch_to(preheader);
        let fresh = self.builder.emit(Type::vector(result_item), Op::NewVector(result_item, count));
        let fresh = if name == "vapply" {
            fresh
        } else {
            self.builder.emit(output_ty, Op::Relabel(fresh, output_ty))
        };
        self.builder.push(Some(output), Op::Copy(fresh));
        self.builder.switch_to(exit);
        Ok(output)
    }

    /// The formals and body of a function argument: a literal or a local function.
    unsafe fn function_argument(&mut self, function: Sexp) -> Result<(Sexp, Sexp), Refusal> {
        if function.kind() == SexpType::Symbol {
            return match self.lookup(function) {
                Some(Binding::Function(formals, body)) => Ok((formals, body)),
                _ => Err(Refusal("a mapped function that is not a local function")),
            };
        }
        if function.kind() == SexpType::Language
            && function.car().kind() == SexpType::Symbol
            && rvalues::symbol_name(function.car()) == "function"
        {
            let parts = rvalues::pairlist_items(function.cdr());
            if let (Some(&(_, formals)), Some(&(_, body))) = (parts.first(), parts.get(1)) {
                return Ok((formals, body));
            }
        }
        Err(Refusal("a mapped function that is not a function literal"))
    }

    unsafe fn paste(&mut self, paste0: bool, arguments: &Arguments) -> Result<Local, Refusal> {
        if !arguments.only_names(&["sep", "collapse"]) {
            return Err(Refusal("a `paste` with unsupported arguments"));
        }
        let separator = match (paste0, arguments.named("sep")) {
            (true, Some(_)) => return Err(Refusal("a `paste0` with `sep`")),
            (true, None) => String::new(),
            (false, None) => String::from(" "),
            (false, Some(separator)) => constant_string(separator).ok_or(Refusal("a computed `paste` separator"))?,
        };
        let collapse = match arguments.named("collapse") {
            Some(collapse) => Some(constant_string(collapse).ok_or(Refusal("a computed `paste` collapse"))?),
            None => None,
        };
        let mut parts: SmallVec<[Local; 4]> = SmallVec::new();
        let mut vectors = false;
        for part in arguments.positional() {
            let value = self.value(part)?;
            let value = self.as_string(value)?;
            vectors |= self.builder.ty(value).items().is_some();
            parts.push(value);
        }
        if parts.is_empty() {
            return Err(Refusal("a `paste` of nothing"));
        }
        let separator = self.string(&separator);
        let pasted = if vectors {
            let mut vector_parts = SmallVec::new();
            for part in parts {
                vector_parts.push(self.as_vector(part)?);
            }
            self.builder
                .emit(Type::vector(Item::Str), Op::PasteVectors(vector_parts, separator))
        } else if collapse.is_some() {
            let mut vector_parts = SmallVec::new();
            for part in parts {
                vector_parts.push(self.as_vector(part)?);
            }
            self.builder
                .emit(Type::vector(Item::Str), Op::PasteVectors(vector_parts, separator))
        } else {
            self.builder.emit(Type::Str, Op::Concat(parts, separator))
        };
        match collapse {
            Some(collapse) => {
                let collapse = self.string(&collapse);
                Ok(self.builder.emit(Type::Str, Op::Join(pasted, collapse)))
            }
            None => Ok(pasted),
        }
    }

    /// A scalar as a vector of one item.
    unsafe fn as_vector(&mut self, value: Local) -> Result<Local, Refusal> {
        let ty = self.builder.ty(value);
        match ty {
            Type::Vector { .. } => Ok(value),
            Type::List(item) => Ok(self.builder.emit(Type::vector(item), Op::Relabel(value, Type::vector(item)))),
            _ => {
                let item = ty.item().ok_or(Refusal("a value that is not a scalar"))?;
                let value = self.convert_scalar(value, item)?;
                let one = self.int(1);
                Ok(self.builder.emit(Type::vector(item), Op::Repeat(value, one)))
            }
        }
    }

    /// A number, string or vector of them as strings, as `paste` and `as.character` give them.
    unsafe fn as_string(&mut self, value: Local) -> Result<Local, Refusal> {
        match self.builder.ty(value) {
            Type::Str | Type::Vector { item: Item::Str, .. } => Ok(value),
            Type::Int | Type::Dbl => Ok(self.builder.emit(Type::Str, Op::Format(value))),
            Type::Vector { item: Item::Int | Item::Dbl, .. } => {
                Ok(self.builder.emit(Type::vector(Item::Str), Op::Coerce(value, Item::Str)))
            }
            _ => Err(Refusal("a value `paste` cannot turn into a string here")),
        }
    }

    unsafe fn as_double(&mut self, value: Local) -> Result<Local, Refusal> {
        match self.builder.ty(value) {
            Type::Dbl | Type::Vector { item: Item::Dbl, .. } => Ok(value),
            Type::Int | Type::Lgl | Type::Bool => Ok(self.builder.emit(Type::Dbl, Op::ToDbl(value))),
            Type::Vector { item: Item::Int | Item::Lgl, .. } => {
                Ok(self.builder.emit(Type::vector(Item::Dbl), Op::Coerce(value, Item::Dbl)))
            }
            _ => Err(Refusal("a value that is not a number")),
        }
    }

    unsafe fn reduce(&mut self, reduce: Reduce, value: Local) -> Result<Local, Refusal> {
        let ty = self.builder.ty(value);
        match ty {
            Type::Int | Type::Dbl if matches!(reduce, Reduce::Sum | Reduce::Prod | Reduce::Max | Reduce::Min) => {
                if reduce == Reduce::Prod && ty == Type::Int {
                    return Ok(self.builder.emit(Type::Dbl, Op::ToDbl(value)));
                }
                Ok(value)
            }
            Type::Lgl if reduce == Reduce::Sum => Ok(self.builder.emit(Type::Int, Op::LglToInt(value))),
            Type::Vector { item, .. } => {
                let (item, result) = match (item, reduce) {
                    (Item::Str, _) => return Err(Refusal("a reduction of strings")),
                    (_, Reduce::Mean | Reduce::Prod) => (Item::Dbl, Type::Dbl),
                    (Item::Dbl, _) => (Item::Dbl, Type::Dbl),
                    (Item::Int | Item::Lgl, Reduce::Sum) => (item, Type::Int),
                    (Item::Int, Reduce::Max | Reduce::Min) => (Item::Int, Type::Int),
                    _ => return Err(Refusal("a reduction the typed core does not know")),
                };
                let value = if item == Item::Dbl { self.as_double(value)? } else { value };
                Ok(self.builder.emit(result, Op::Reduce(reduce, item, value)))
            }
            _ => Err(Refusal("a reduction of something other than numbers")),
        }
    }

    unsafe fn math(&mut self, op: Math, value: Local) -> Result<Local, Refusal> {
        let ty = self.builder.ty(value);
        let keeps_integers = matches!(op, Math::Neg | Math::Abs);
        match ty {
            Type::Int if keeps_integers => Ok(self.builder.emit(Type::Int, Op::Math(op, Item::Int, value))),
            Type::Lgl if keeps_integers => {
                let value = self.builder.emit(Type::Int, Op::LglToInt(value));
                Ok(self.builder.emit(Type::Int, Op::Math(op, Item::Int, value)))
            }
            Type::Int | Type::Lgl | Type::Dbl => {
                let value = self.as_double(value)?;
                Ok(self.builder.emit(Type::Dbl, Op::Math(op, Item::Dbl, value)))
            }
            Type::Vector { item: Item::Int, .. } if keeps_integers => {
                Ok(self.builder.emit(Type::vector(Item::Int), Op::VectorMath(op, Item::Int, value)))
            }
            Type::Vector { item: Item::Int | Item::Lgl | Item::Dbl, .. } => {
                let value = self.as_double(value)?;
                Ok(self.builder.emit(Type::vector(Item::Dbl), Op::VectorMath(op, Item::Dbl, value)))
            }
            _ => Err(Refusal("a math function of something other than numbers")),
        }
    }

    unsafe fn arith(&mut self, op: Arith, a: Local, b: Local) -> Result<Local, Refusal> {
        let (ta, tb) = (self.builder.ty(a), self.builder.ty(b));
        let numeric_item = |ty: Type| match ty {
            Type::Int | Type::Lgl | Type::Bool | Type::Vector { item: Item::Int | Item::Lgl, .. } => Some(Item::Int),
            Type::Dbl | Type::Vector { item: Item::Dbl, .. } => Some(Item::Dbl),
            _ => None,
        };
        let (Some(ia), Some(ib)) = (numeric_item(ta), numeric_item(tb)) else {
            return Err(Refusal("arithmetic on something other than numbers"));
        };
        // Integers divide and raise to powers as doubles in R.
        let item = if ia == Item::Dbl || ib == Item::Dbl || matches!(op, Arith::Div | Arith::Pow) {
            Item::Dbl
        } else {
            Item::Int
        };
        let a = self.operand(a, item)?;
        let b = self.operand(b, item)?;
        let vectors = ta.items().is_some() || tb.items().is_some();
        if vectors {
            Ok(self.builder.emit(Type::vector(item), Op::VectorArith(op, item, a, b)))
        } else {
            Ok(self.builder.emit(Type::of_item(item), Op::Arith(op, item, a, b)))
        }
    }

    /// A numeric operand converted to `item`, as a scalar or a vector as it was.
    unsafe fn operand(&mut self, value: Local, item: Item) -> Result<Local, Refusal> {
        let ty = self.builder.ty(value);
        match (ty, item) {
            (Type::Vector { item: Item::Dbl, .. }, Item::Dbl) | (Type::Vector { item: Item::Int, .. }, Item::Int) => Ok(value),
            (Type::Vector { item: from, .. }, _) if from != item => {
                Ok(self.builder.emit(Type::vector(item), Op::Coerce(value, item)))
            }
            (Type::Bool, _) => {
                let lgl = self.builder.emit(Type::Lgl, Op::Copy(value));
                self.convert_scalar(lgl, item)
            }
            _ => self.convert_scalar(value, item),
        }
    }

    unsafe fn compare(&mut self, op: Compare, a: Local, b: Local) -> Result<Local, Refusal> {
        let (ta, tb) = (self.builder.ty(a), self.builder.ty(b));
        let vectors = ta.items().is_some() || tb.items().is_some();
        let item_of = |ty: Type| ty.item().or_else(|| ty.items());
        let (Some(ia), Some(ib)) = (item_of(ta), item_of(tb)) else {
            return Err(Refusal("a comparison of something other than scalars or vectors"));
        };
        let item = match (ia, ib) {
            (Item::Str, Item::Str) => {
                if !matches!(op, Compare::Eq | Compare::Ne) {
                    return Err(Refusal("an ordering comparison of strings, which depends on the locale"));
                }
                Item::Str
            }
            (Item::Str, _) | (_, Item::Str) => return Err(Refusal("a comparison of a string and a number")),
            (Item::Dbl, _) | (_, Item::Dbl) => Item::Dbl,
            _ => Item::Int,
        };
        let (a, b) = if item == Item::Str { (a, b) } else { (self.operand(a, item)?, self.operand(b, item)?) };
        if vectors {
            Ok(self.builder.emit(Type::vector(Item::Lgl), Op::VectorCompare(op, item, a, b)))
        } else {
            Ok(self.builder.emit(Type::Lgl, Op::Compare(op, item, a, b)))
        }
    }
}

/// The item a `vapply` template such as `numeric(1)` declares.
unsafe fn template_item(template: Sexp) -> Option<Item> {
    if template.kind() != SexpType::Language || template.car().kind() != SexpType::Symbol {
        return None;
    }
    let item = match &*rvalues::symbol_name(template.car()) {
        "numeric" | "double" => Item::Dbl,
        "integer" => Item::Int,
        "logical" => Item::Lgl,
        "character" => Item::Str,
        _ => return None,
    };
    let arguments = rvalues::pairlist_items(template.cdr());
    match arguments[..] {
        [(_, length)] if constant_integer(length) == Some(1) => Some(item),
        _ => None,
    }
}

/// An integer constant, or a double constant that is a whole number.
unsafe fn constant_integer(value: Sexp) -> Option<i32> {
    if value.attrib() != rvalues::nil() || rvalues::length(value) != 1 {
        return None;
    }
    match value.kind() {
        SexpType::Integer => Some(rvalues::ints(value)[0]).filter(|&value| value != i32::MIN),
        SexpType::Real => {
            let number = rvalues::reals(value)[0];
            (number.fract() == 0.0 && number.abs() < 1e9).then_some(number as i32)
        }
        _ => None,
    }
}

unsafe fn constant_string(value: Sexp) -> Option<String> {
    if value.kind() != SexpType::String || rvalues::length(value) != 1 || value.attrib() != rvalues::nil() {
        return None;
    }
    rvalues::string_at(value, 0).map(|text| text.into_owned())
}

/// Every symbol an assignment in `expression` writes, `for` variables included, outside nested
/// function literals.
unsafe fn assigned_symbols(expression: Sexp, out: &mut Vec<Sexp>) {
    if expression.kind() != SexpType::Language {
        return;
    }
    let head = expression.car();
    let items = rvalues::pairlist_items(expression.cdr());
    if head.kind() == SexpType::Symbol {
        match &*rvalues::symbol_name(head) {
            "function" => return,
            "<-" | "=" => {
                if let Some(&(_, mut target)) = items.first() {
                    while target.kind() == SexpType::Language {
                        match rvalues::pairlist_items(target.cdr()).first() {
                            Some(&(_, inner)) => target = inner,
                            None => break,
                        }
                    }
                    if target.kind() == SexpType::Symbol {
                        out.push(target);
                    }
                }
            }
            "for" => {
                if let Some(&(_, variable)) = items.first() {
                    out.push(variable);
                }
            }
            _ => {}
        }
    }
    for (_, item) in items {
        assigned_symbols(item, out);
    }
}

/// Whether `expression` calls `return` outside nested function literals.
unsafe fn contains_return(expression: Sexp) -> bool {
    if expression.kind() != SexpType::Language {
        return false;
    }
    let head = expression.car();
    if head.kind() == SexpType::Symbol {
        match &*rvalues::symbol_name(head) {
            "return" => return true,
            "function" => return false,
            _ => {}
        }
    }
    rvalues::pairlist_items(expression.cdr())
        .into_iter()
        .any(|(_, item)| contains_return(item))
}

/// Whether the value of `expression` may be invisible, as an assignment's is, which a compiled
/// call could not reproduce.
pub unsafe fn tail_invisible(expression: Sexp) -> bool {
    if expression.kind() != SexpType::Language || expression.car().kind() != SexpType::Symbol {
        return false;
    }
    let args = rvalues::pairlist_items(expression.cdr());
    match &*rvalues::symbol_name(expression.car()) {
        "<-" | "=" | "invisible" | "for" | "while" | "repeat" => true,
        "{" => args.last().is_some_and(|&(_, last)| tail_invisible(last)),
        "if" => args.iter().skip(1).any(|&(_, branch)| tail_invisible(branch)),
        _ => false,
    }
}

/// Whether `function` is base R's or the standard library's own function of this name.
unsafe fn is_base_function(speculation: &Speculation) -> bool {
    let function = speculation.function;
    match function.kind() {
        SexpType::Builtin | SexpType::Special => interp::eval::builtin_def(function)
            .is_ok_and(|def| rvalues::symbol_name(speculation.symbol) == def.name),
        // Base binds most of its closures to lazy-load promises, so the cell, not its value,
        // identifies the binding.
        SexpType::Closure => {
            let heap = interp::heap();
            heap.frame_cell(heap.known().base_namespace, speculation.symbol) == Some(speculation.cell)
        }
        _ => false,
    }
}

/// Walks from `start` to the binding a function lookup of `symbol` finds, as the interpreter
/// does, but gives up instead of forcing a promise or calling an active binding.
pub unsafe fn resolve_function(symbol: Sexp, start: Sexp) -> Option<Speculation> {
    let heap = interp::heap();
    let mut env = start;
    let empty = heap.known().empty_env;
    while env != empty {
        if let Some(cell) = heap.frame_cell(env, symbol) {
            if cell.has_gp(heap::object::gp::ACTIVE_BINDING) {
                return None;
            }
            let mut value = cell.cdr();
            if value.kind() == SexpType::Promise {
                if value.car() == rvalues::unbound() {
                    // Base binds its closures to lazy-load promises, which only read the
                    // function from the package database, so forcing one is safe. A promise
                    // anywhere else may run arbitrary code. A load that fails leaves the call to
                    // the interpreter, which raises the error itself.
                    if env != heap.known().base_env && env != heap.known().base_namespace {
                        return None;
                    }
                    value = interp::eval::force_promise(value).ok()?;
                } else {
                    value = value.car();
                }
            }
            if !rvalues::is_function(value) {
                return None;
            }
            return Some(Speculation {
                symbol,
                env: start,
                cell,
                function: value,
            });
        }
        env = env.enclosure();
    }
    None
}

/// Logs the name a refusal is about, since the refusal itself is a fixed message.
fn note(name: &str) {
    if crate::logging() {
        eprintln!("typed: refusing `{name}`");
    }
}
