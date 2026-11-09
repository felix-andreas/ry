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
        return Err(Refusal(
            "the call does not supply every formal positionally",
        ));
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
        control: 0,
        unusable: SmallVec::new(),
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
        lowering.usable(value)?;
        // `-> I64` converts a whole R number, as the interpreter's return check does.
        let value = if interp::annotations::returns_i64(closure)
            && matches!(lowering.builder.ty(value), Type::Int | Type::Dbl)
        {
            lowering.to_i64(value)?
        } else {
            value
        };
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
    /// A local function: its formals, its body and the index of the scope that defines it, whose
    /// names are the only ones the body sees when it is inlined.
    Function(Sexp, Sexp, usize),
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
    /// How many branches and loop bodies enclose the current point in the current scope.
    control: usize,
    /// The results of an `if` whose branches have different types, which nothing may read.
    unusable: SmallVec<[Local; 4]>,
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
                    let name =
                        (tag != rvalues::nil()).then(|| rvalues::symbol_name(tag).into_owned());
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
        let value = self
            .expression(expression)?
            .ok_or(Refusal("an argument that never completes"))?;
        self.usable(value)?;
        Ok(value)
    }

    fn usable(&self, value: Local) -> Result<(), Refusal> {
        if self.unusable.contains(&value) {
            return Err(Refusal(
                "a value whose type depends on the branch an `if` takes",
            ));
        }
        Ok(())
    }

    /// Enters a branch or a loop body, which may not run, giving the current scope's names to
    /// hand back to [`Lowering::leave_control`].
    fn enter_control(&mut self) -> FxHashMap<Sexp, Binding> {
        self.control += 1;
        self.scopes
            .last()
            .map(|scope| scope.names.clone())
            .unwrap_or_default()
    }

    /// Leaves a branch or loop body: the names it created go out of scope, since they are unset
    /// on a path that skips it, and come back as a list for an `if` to join.
    fn leave_control(&mut self, saved: FxHashMap<Sexp, Binding>) -> SmallVec<[(Sexp, Local); 4]> {
        self.control -= 1;
        let Some(scope) = self.scopes.last_mut() else {
            return SmallVec::new();
        };
        let mut created: SmallVec<[(Sexp, Local); 4]> = scope
            .names
            .iter()
            .filter(|(symbol, _)| !saved.contains_key(symbol))
            .filter_map(|(&symbol, &binding)| match binding {
                Binding::Value(local) => Some((symbol, local)),
                Binding::Function(..) => None,
            })
            .collect();
        // The join emits copies in this order, which must not depend on hashing.
        created.sort_by_key(|&(_, local)| local);
        scope.names = saved;
        created
    }

    unsafe fn expression_inner(&mut self, expression: Sexp) -> Result<Option<Local>, Refusal> {
        match expression.kind() {
            SexpType::Integer
            | SexpType::Real
            | SexpType::Logical
            | SexpType::String
            | SexpType::I64
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
        if let Some(number) = interp::numbers::read(value) {
            return Ok(self
                .builder
                .emit(Type::I64, Op::Const(Constant::I64(number))));
        }
        let (ty, constant) = match value.kind() {
            SexpType::Integer => (Type::Int, Constant::Int(rvalues::ints(value)[0])),
            SexpType::Logical => (Type::Lgl, Constant::Lgl(rvalues::ints(value)[0])),
            SexpType::Real => (Type::Dbl, Constant::Dbl(rvalues::reals(value)[0])),
            SexpType::String => {
                let text = rvalues::string_at(value, 0).ok_or(Refusal("an NA string constant"))?;
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
            None => self.library_constant(symbol),
        }
    }

    /// A constant of the standard library, such as `Num.pi`, read as its value for as long as
    /// the lookup finds that binding.
    unsafe fn library_constant(&mut self, symbol: Sexp) -> Result<Local, Refusal> {
        // `package:std` binds the very object the builtins environment was created with, and
        // that object is shared, so nothing modifies it in place.
        let original = interp::standard_library::builtins_env()
            .and_then(|env| interp::heap().frame_cell(env, symbol))
            .map(|cell| cell.cdr());
        let found = resolve_value(symbol, self.env)
            .filter(|speculation| original == Some(speculation.function));
        let Some(speculation) = found else {
            note(&rvalues::symbol_name(symbol));
            return Err(Refusal("a variable that is not a local"));
        };
        // Locals are bound as lowering reaches their assignment, so a read that a loop runs
        // again after a later assignment of the same name would keep reading the constant.
        if assigns(interp::bytecode::closure_body(self.closure), symbol) {
            return Err(Refusal("a library constant the function assigns"));
        }
        let local = self.constant(speculation.function)?;
        self.speculations.push(speculation);
        Ok(local)
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
                if self.control > 0
                    && matches!(
                        self.scopes
                            .last()
                            .and_then(|scope| scope.names.get(&symbol)),
                        Some(Binding::Function(..))
                    )
                {
                    return Err(Refusal("a local function rebound in a branch or loop"));
                }
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
                let target = if name == "break" {
                    break_to
                } else {
                    continue_to
                };
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
        if let Some(Binding::Function(formals, body, scope)) = self.lookup(head) {
            let arguments = Arguments::of(call);
            let values = self.call_arguments(&arguments)?;
            return self.inline(formals, body, scope, &values);
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
            let types: SmallVec<[Type; 4]> =
                values.iter().map(|&value| self.builder.ty(value)).collect();
            let (index, result) = if function == self.closure && types[..] == self.params[..] {
                (crate::lower::SELF, self.guess)
            } else {
                self.callees.callee(function, &types)?
            };
            return Ok(Some(
                self.builder.emit(result, Op::CallTyped(index, values)),
            ));
        }
        note(&name);
        Err(Refusal("a call of a function the typed core does not know"))
    }

    unsafe fn call_arguments(
        &mut self,
        arguments: &Arguments,
    ) -> Result<SmallVec<[Local; 4]>, Refusal> {
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
                let value = if self.guess == Type::I64
                    && interp::annotations::returns_i64(self.closure)
                    && matches!(self.builder.ty(value), Type::Int | Type::Dbl)
                {
                    self.to_i64(value)?
                } else {
                    value
                };
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
            return Ok(Some(
                self.builder.emit(Type::Null, Op::Const(Constant::Null)),
            ));
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
                if self.control > 0 {
                    return Err(Refusal("a local function defined in a branch or loop"));
                }
                let scope = self.scopes.len() - 1;
                self.bind(target, Binding::Function(formals, body, scope));
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
                let variable = self.lookup_assignable(vector).ok_or(Refusal(
                    "an indexed assignment into a parameter or a non-local",
                ))?;
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
            Type::Int | Type::I64 => Ok(index),
            // A whole double indexes as that integer. A fraction gives up, so the interpreter
            // truncates it as R does.
            Type::Dbl => self.to_i64(index),
            _ => Err(Refusal("an index that is not an integer")),
        }
    }

    /// `value` as a scalar of `item`, converting a narrower number as R would on assignment.
    unsafe fn convert_scalar(&mut self, value: Local, item: Item) -> Result<Local, Refusal> {
        let ty = self.builder.ty(value);
        match (ty, item) {
            (Type::Int, Item::Int)
            | (Type::I64, Item::I64)
            | (Type::Dbl, Item::Dbl)
            | (Type::Lgl, Item::Lgl)
            | (Type::Str, Item::Str) => Ok(value),
            (Type::Int | Type::Lgl, Item::Dbl) => {
                Ok(self.builder.emit(Type::Dbl, Op::ToDbl(value)))
            }
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
        let saved = self.enter_control();
        let then_value = self.expression(then)?;
        let then_created = self.leave_control(saved);
        let then_end = self.builder.current;
        self.builder.switch_to(else_block);
        let saved = self.enter_control();
        let else_value = match otherwise {
            Some(otherwise) => self.expression(otherwise)?,
            None => Some(self.builder.emit(Type::Null, Op::Const(Constant::Null))),
        };
        let else_created = self.leave_control(saved);
        let else_end = self.builder.current;
        // A name the `if` creates is set after it when every branch that completes sets it,
        // with the same type.
        match (then_value.is_some(), else_value.is_some()) {
            (true, false) => then_created
                .iter()
                .for_each(|&(symbol, local)| self.bind(symbol, Binding::Value(local))),
            (false, true) => else_created
                .iter()
                .for_each(|&(symbol, local)| self.bind(symbol, Binding::Value(local))),
            (true, true) => {
                for &(symbol, then_local) in &then_created {
                    let Some(&(_, else_local)) =
                        else_created.iter().find(|&&(other, _)| other == symbol)
                    else {
                        continue;
                    };
                    let ty = self.builder.ty(then_local);
                    if self.builder.ty(else_local) != ty {
                        continue;
                    }
                    let joined = self.builder.local(ty);
                    self.builder.switch_to(then_end);
                    self.builder.push(Some(joined), Op::Copy(then_local));
                    self.builder.switch_to(else_end);
                    self.builder.push(Some(joined), Op::Copy(else_local));
                    self.bind(symbol, Binding::Value(joined));
                }
            }
            (false, false) => {}
        }
        let result = match (then_value, else_value) {
            (None, None) => None,
            (Some(value), None) | (None, Some(value)) => {
                let result = self.builder.local(self.builder.ty(value));
                let end = if then_value.is_some() {
                    then_end
                } else {
                    else_end
                };
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
                    self.unusable.push(result);
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
        // A counted loop over `seq_len(n)`, `seq_along(x)`, `a:b` or `List.range(a, b)`
        // allocates nothing.
        let (start, end, step, items) = match self.counted_range(sequence)? {
            Some((start, end, step)) => (start, end, step, None),
            None => {
                let vector = self.value(sequence)?;
                let ty = self.builder.ty(vector);
                ty.items()
                    .ok_or(Refusal("a `for` over something that is not a vector"))?;
                // The loop reads the value the sequence had when it started.
                let held = self.builder.emit(ty, Op::Copy(vector));
                let one = self.int(1);
                let length = self.builder.emit(Type::Int, Op::Length(held));
                let step = self.int(1);
                (one, length, Step::FromOne(step), Some(held))
            }
        };
        let counter_ty = self.builder.ty(start);
        let counter = self.builder.emit(counter_ty, Op::Copy(start));
        // The body may assign the variable that held the end, which must not move it.
        let end = self.builder.emit(counter_ty, Op::Copy(end));
        let exit = self.builder.block();
        let step = match step {
            // `List.range(a, b)` counts up from `a` to `b` and is empty when `b < a`.
            Step::Inclusive(step) => {
                let empty = self
                    .builder
                    .emit(Type::Bool, Op::Compare(Compare::Lt, Item::I64, end, start));
                let go = self.builder.block();
                self.builder.terminate(Terminator::Branch {
                    condition: empty,
                    then: exit,
                    otherwise: go,
                });
                self.builder.switch_to(go);
                step
            }
            // `seq_len`, `seq_along` and a vector count up from 1 to an end that is not `NA` and
            // not negative, and run no iteration when it is 0.
            Step::FromOne(step) => {
                let zero = self.int(0);
                let empty = self.builder.emit(Type::Bool, Op::Same(end, zero));
                let go = self.builder.block();
                self.builder.terminate(Terminator::Branch {
                    condition: empty,
                    then: exit,
                    otherwise: go,
                });
                self.builder.switch_to(go);
                step
            }
            // `a:b` always holds at least one item and counts down when `b < a`. The comparison
            // fails on an `NA` end, as `:` does.
            Step::Signed => {
                let down = self
                    .builder
                    .emit(Type::Lgl, Op::Compare(Compare::Lt, Item::Int, end, start));
                let down = self.builder.emit(Type::Bool, Op::Truth(down));
                let minus = self.int(-1);
                let plus = self.int(1);
                let step = self.builder.local(Type::Int);
                let (a, b, join) = (
                    self.builder.block(),
                    self.builder.block(),
                    self.builder.block(),
                );
                self.builder.terminate(Terminator::Branch {
                    condition: down,
                    then: a,
                    otherwise: b,
                });
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
        let saved = self.enter_control();
        let item = match items {
            Some(held) => self.builder.emit(
                Type::of_item(
                    self.builder
                        .ty(held)
                        .items()
                        .ok_or(Refusal("a loop over a non-vector"))?,
                ),
                Op::Get(held, counter),
            ),
            None => counter,
        };
        self.assign(variable, item)?;
        let latch = self.builder.block();
        self.loops.push((latch, exit));
        let completed = self.expression(body)?;
        self.loops.pop();
        self.leave_control(saved);
        if completed.is_some() {
            self.builder.terminate(Terminator::Jump(latch));
        }
        self.builder.switch_to(latch);
        let done = self.builder.emit(Type::Bool, Op::Same(counter, end));
        let next_block = self.builder.block();
        self.builder.terminate(Terminator::Branch {
            condition: done,
            then: exit,
            otherwise: next_block,
        });
        self.builder.switch_to(next_block);
        let next = self.builder.emit(counter_ty, Op::Step(counter, step));
        self.builder.push(Some(counter), Op::Copy(next));
        self.builder.terminate(Terminator::Jump(header));
        self.builder.switch_to(exit);
        Ok(Some(
            self.builder.emit(Type::Null, Op::Const(Constant::Null)),
        ))
    }

    /// The start, end and step of a counted sequence.
    unsafe fn counted_range(
        &mut self,
        sequence: Sexp,
    ) -> Result<Option<(Local, Local, Step)>, Refusal> {
        if sequence.kind() != SexpType::Language || sequence.car().kind() != SexpType::Symbol {
            return Ok(None);
        }
        let head = sequence.car();
        if self.lookup(head).is_some() {
            return Ok(None);
        }
        let name = rvalues::symbol_name(head).into_owned();
        if !matches!(name.as_str(), "seq_len" | "seq_along" | ":" | "List.range") {
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
                    let negative = self
                        .builder
                        .emit(Type::Lgl, Op::Compare(Compare::Lt, Item::Int, count, zero));
                    self.fail_if(negative);
                    count
                } else {
                    self.length(value)
                };
                let one = self.int(1);
                let step = self.int(1);
                Ok(Some((one, end, Step::FromOne(step))))
            }
            "List.range" => {
                let [from, to] = self.arguments(sequence, 2)?[..] else {
                    return Err(Refusal("a malformed `List.range`"));
                };
                let from = self.value(from)?;
                let to = self.value(to)?;
                let from = self.to_i64(from)?;
                let to = self.to_i64(to)?;
                // The loop allocates no list, but `List.range` refuses one longer than R's
                // vector limit, so a range that long fails here as it does in the interpreter.
                let span = self
                    .builder
                    .emit(Type::I64, Op::Arith(Arith::Sub, Item::I64, to, from));
                let limit = self
                    .builder
                    .emit(Type::I64, Op::Const(Constant::I64(i64::from(i32::MAX))));
                let too_long = self
                    .builder
                    .emit(Type::Bool, Op::Compare(Compare::Ge, Item::I64, span, limit));
                let (fail, go) = (self.builder.block(), self.builder.block());
                self.builder.terminate(Terminator::Branch {
                    condition: too_long,
                    then: fail,
                    otherwise: go,
                });
                self.builder.switch_to(fail);
                self.builder.terminate(Terminator::Fail);
                self.builder.switch_to(go);
                let step = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
                Ok(Some((from, to, Step::Inclusive(step))))
            }
            _ => {
                let [from, to] = self.arguments(sequence, 2)?[..] else {
                    return Err(Refusal("a malformed `:`"));
                };
                let from = self.value(from)?;
                let to = self.value(to)?;
                let from = self.range_end(from)?;
                let to = self.range_end(to)?;
                Ok(Some((from, to, Step::Signed)))
            }
        }
    }

    /// Fails the call when `condition`, a logical, is true or `NA`.
    fn fail_if(&mut self, condition: Local) {
        let truth = self.builder.emit(Type::Bool, Op::Truth(condition));
        let (fail, go) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: truth,
            then: fail,
            otherwise: go,
        });
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
                let constant = self
                    .constant_double(value)
                    .ok_or(Refusal("a `:` of computed doubles"))?;
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

    /// A number as an `I64`, converting an R integer or double exactly as the standard library
    /// does: the call fails on `NA` or a fractional double, where the interpreter raises the
    /// error. A whole double constant converts at compile time.
    unsafe fn to_i64(&mut self, value: Local) -> Result<Local, Refusal> {
        match self.builder.ty(value) {
            Type::I64 => Ok(value),
            Type::Dbl => match self.constant_double(value) {
                Some(constant)
                    if constant.fract() == 0.0
                        && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0)
                            .contains(&constant) =>
                {
                    Ok(self
                        .builder
                        .emit(Type::I64, Op::Const(Constant::I64(constant as i64))))
                }
                _ => Ok(self.builder.emit(Type::I64, Op::ToI64(value))),
            },
            Type::Int => Ok(self.builder.emit(Type::I64, Op::ToI64(value))),
            _ => Err(Refusal("an I64 from something other than a number")),
        }
    }

    /// Fails the call when `value`, a double, is `NaN`, which includes `NA`.
    fn fail_if_nan(&mut self, value: Local) {
        let unordered = self
            .builder
            .emit(Type::Lgl, Op::Compare(Compare::Ne, Item::Dbl, value, value));
        self.fail_if(unordered);
    }

    /// A count argument (`seq_len(n)`, `numeric(n)`) as an integer.
    unsafe fn integer_count(&mut self, value: Local) -> Result<Local, Refusal> {
        match self.builder.ty(value) {
            Type::Int => Ok(value),
            Type::Dbl => {
                let constant = self
                    .constant_double(value)
                    .ok_or(Refusal("a count that is a computed double"))?;
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
        self.builder.terminate(Terminator::Branch {
            condition,
            then: body_block,
            otherwise: exit,
        });
        self.builder.switch_to(body_block);
        self.loops.push((header, exit));
        let saved = self.enter_control();
        let completed = self.expression(body)?;
        self.leave_control(saved);
        self.loops.pop();
        if completed.is_some() {
            self.builder.terminate(Terminator::Jump(header));
        }
        self.builder.switch_to(exit);
        Ok(Some(
            self.builder.emit(Type::Null, Op::Const(Constant::Null)),
        ))
    }

    unsafe fn repeat_loop(&mut self, call: Sexp) -> Result<Option<Local>, Refusal> {
        let [body] = self.arguments(call, 1)?[..] else {
            return Err(Refusal("a malformed `repeat`"));
        };
        let header = self.loop_header();
        let exit = self.builder.block();
        self.loops.push((header, exit));
        let saved = self.enter_control();
        let completed = self.expression(body)?;
        self.leave_control(saved);
        self.loops.pop();
        if completed.is_some() {
            self.builder.terminate(Terminator::Jump(header));
        }
        self.builder.switch_to(exit);
        Ok(Some(
            self.builder.emit(Type::Null, Op::Const(Constant::Null)),
        ))
    }

    /// Inlines a local function or function literal called with `arguments`.
    unsafe fn inline(
        &mut self,
        formals: Sexp,
        body: Sexp,
        scope_index: usize,
        arguments: &[Local],
    ) -> Result<Option<Local>, Refusal> {
        if self.inlined.len() >= MAX_INLINE_DEPTH {
            return Err(Refusal("local functions inline too deeply"));
        }
        let definition = formals;
        let formals = rvalues::pairlist_items(formals);
        if formals.len() != arguments.len() {
            return Err(Refusal(
                "a local function called with another number of arguments",
            ));
        }
        // The arguments meet the function's annotations as a call's would: an `I64` formal
        // converts a whole R number, and every other check must pass for the types at hand.
        let (i64_formals, returns_i64) = interp::annotations::declared_i64(definition, body);
        let mut converted: SmallVec<[Local; 4]> = SmallVec::new();
        for (index, &argument) in arguments.iter().enumerate() {
            converted.push(if i64_formals.get(index).copied().unwrap_or(false) {
                self.to_i64(argument)?
            } else {
                argument
            });
        }
        let arguments = &converted[..];
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
        // The body sees the names of the scope that defines the function, as R's lexical scoping
        // does, not those of the inlined functions that call it. A `break` or `next` in it does
        // not reach the caller's loops, and its own branches start at its own top level.
        let hidden = self
            .scopes
            .split_off((scope_index + 1).min(self.scopes.len()));
        let loops = std::mem::take(&mut self.loops);
        let control = std::mem::replace(&mut self.control, 0);
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
        self.scopes.extend(hidden);
        self.loops = loops;
        self.control = control;
        let value = value?;
        match value {
            Some(value) => {
                self.usable(value)?;
                let value = if returns_i64 {
                    self.to_i64(value)?
                } else {
                    value
                };
                let kinds: SmallVec<[Option<SexpType>; 4]> = arguments
                    .iter()
                    .map(|&argument| plain_storage(self.builder.ty(argument)))
                    .collect();
                let result_kind = plain_storage(self.builder.ty(value));
                if !interp::annotations::admits_inline(definition, body, &kinds, result_kind) {
                    return Err(Refusal(
                        "a local function whose annotations reject its types",
                    ));
                }
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
        self.builder
            .emit(Type::Int, Op::Const(Constant::Int(value)))
    }

    fn length(&mut self, value: Local) -> Local {
        match self.builder.ty(value) {
            Type::Null => self.int(0),
            ty if ty.items().is_some() => self.builder.emit(Type::Int, Op::Length(value)),
            _ => self.int(1),
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
                        if matches!(self.builder.ty(value), Type::Int | Type::Dbl | Type::I64) {
                            Ok(value)
                        } else {
                            Err(Refusal("a unary `+` of a non-number"))
                        }
                    }
                    [a, b] => {
                        let mut nodes = SmallVec::new();
                        let left = self.arith_tree(a, &mut nodes)?;
                        let right = self.arith_tree(b, &mut nodes)?;
                        nodes.push(Node::Arith(arith_op(name), left, right));
                        self.arith_nodes(&nodes, false).map(|(value, _)| value)
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
                    Type::Vector {
                        item: Item::Lgl, ..
                    } => Ok(self.builder.emit(Type::vector(Item::Lgl), Op::VectorNot(a))),
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
                    (
                        Type::Vector {
                            item: Item::Lgl, ..
                        }
                        | Type::Lgl,
                        Type::Vector {
                            item: Item::Lgl, ..
                        }
                        | Type::Lgl,
                    ) => {
                        let a = self.as_vector(a)?;
                        let b = self.as_vector(b)?;
                        Ok(self
                            .builder
                            .emit(Type::vector(Item::Lgl), Op::VectorLogic(and, a, b)))
                    }
                    _ => Err(Refusal(
                        "a logical operator on something other than logicals",
                    )),
                }
            }
            "&&" | "||" => {
                let [a, b] = arguments.exactly(2)?[..] else {
                    return Err(Refusal("a malformed `&&`"));
                };
                let a = self.condition(a)?;
                let result = self.builder.local(Type::Lgl);
                let (evaluate, short, join) = (
                    self.builder.block(),
                    self.builder.block(),
                    self.builder.block(),
                );
                let (then, otherwise) = if name == "&&" {
                    (evaluate, short)
                } else {
                    (short, evaluate)
                };
                self.builder.terminate(Terminator::Branch {
                    condition: a,
                    then,
                    otherwise,
                });
                self.builder.switch_to(short);
                let constant = self
                    .builder
                    .emit(Type::Lgl, Op::Const(Constant::Lgl(i32::from(name == "||"))));
                self.builder.push(Some(result), Op::Copy(constant));
                self.builder.terminate(Terminator::Jump(join));
                self.builder.switch_to(evaluate);
                let saved = self.enter_control();
                let b = self.condition(b)?;
                self.leave_control(saved);
                let b = self.builder.emit(Type::Lgl, Op::Copy(b));
                self.builder.function.locals[b.0 as usize] = Type::Lgl;
                self.builder.push(Some(result), Op::Copy(b));
                self.builder.terminate(Terminator::Jump(join));
                self.builder.switch_to(join);
                Ok(result)
            }
            "List.len" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a malformed `List.len`"));
                };
                let a = self.value(a)?;
                match self.builder.ty(a) {
                    Type::Vector { array: true, .. } | Type::List(_) => {
                        Ok(self.builder.emit(Type::I64, Op::Length(a)))
                    }
                    _ => Err(Refusal("a `List.len` of something other than a list")),
                }
            }
            "List.range" => {
                let [from, to] = arguments.exactly(2)?[..] else {
                    return Err(Refusal("a malformed `List.range`"));
                };
                let from = self.value(from)?;
                let to = self.value(to)?;
                let from = self.to_i64(from)?;
                let to = self.to_i64(to)?;
                let ty = Type::Vector {
                    item: Item::I64,
                    array: true,
                };
                Ok(self.builder.emit(ty, Op::RangeI64(from, to)))
            }
            "Num.to_f64" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a malformed `Num.to_f64`"));
                };
                let a = self.value(a)?;
                match self.builder.ty(a) {
                    Type::I64 => Ok(self.builder.emit(Type::Dbl, Op::ToDbl(a))),
                    Type::Int => {
                        let exact = self.builder.emit(Type::I64, Op::ToI64(a));
                        Ok(self.builder.emit(Type::Dbl, Op::ToDbl(exact)))
                    }
                    Type::Dbl => {
                        self.fail_if_nan(a);
                        Ok(a)
                    }
                    _ => Err(Refusal("a `Num.to_f64` of something other than a number")),
                }
            }
            "Num.min" | "Num.max" => {
                let [a, b] = arguments.exactly(2)?[..] else {
                    return Err(Refusal("a malformed `Num.min`"));
                };
                let a = self.value(a)?;
                let b = self.value(b)?;
                let maximum = name == "Num.max";
                self.extreme(maximum, a, b)
            }
            "Num.sqrt" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a malformed `Num.sqrt`"));
                };
                let a = self.value(a)?;
                let a = match self.builder.ty(a) {
                    Type::Int | Type::Dbl => self.as_double(a)?,
                    _ => return Err(Refusal("a `Num.sqrt` of something other than an F64")),
                };
                // A negative number, `NaN` and `NA` are errors, which the interpreter raises.
                let zero = self.builder.emit(Type::Dbl, Op::Const(Constant::Dbl(0.0)));
                let negative = self
                    .builder
                    .emit(Type::Lgl, Op::Compare(Compare::Lt, Item::Dbl, a, zero));
                self.fail_if(negative);
                Ok(self
                    .builder
                    .emit(Type::Dbl, Op::Math(Math::Sqrt, Item::Dbl, a)))
            }
            "Num.pow" => {
                let [base, exponent] = arguments.exactly(2)?[..] else {
                    return Err(Refusal("a malformed `Num.pow`"));
                };
                let base = self.value(base)?;
                let exponent = self.value(exponent)?;
                let mut doubles = [base, exponent];
                for value in &mut doubles {
                    *value = match self.builder.ty(*value) {
                        Type::Int | Type::Dbl => self.as_double(*value)?,
                        _ => return Err(Refusal("a `Num.pow` of something other than F64s")),
                    };
                }
                let [base, exponent] = doubles;
                let power = self.builder.emit(
                    Type::Dbl,
                    Op::Arith(Arith::NumPow, Item::Dbl, base, exponent),
                );
                self.fail_if_nan(power);
                Ok(power)
            }
            "Num.round" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a malformed `Num.round`"));
                };
                let a = self.value(a)?;
                match self.builder.ty(a) {
                    Type::Int => Ok(self.builder.emit(Type::I64, Op::ToI64(a))),
                    Type::Dbl => {
                        let whole = self
                            .builder
                            .emit(Type::Dbl, Op::Math(Math::RoundAway, Item::Dbl, a));
                        Ok(self.builder.emit(Type::I64, Op::ToI64(whole)))
                    }
                    _ => Err(Refusal("a `Num.round` of something other than an F64")),
                }
            }
            "List.count_if" | "List.walk" => self.walk(&name, &arguments),
            "length" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a malformed `length`"));
                };
                let a = self.value(a)?;
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
                Ok(self
                    .builder
                    .emit(Type::vector(Item::Int), Op::Range(from, to)))
            }
            "rep" | "rep.int" | "List.repeat" => {
                if !arguments.only_names(&["x", "times", "value", "count"])
                    || arguments.items.len() != 2
                {
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
                let ty = if name == "List.repeat" {
                    Type::List(item)
                } else {
                    Type::vector(item)
                };
                Ok(self.builder.emit(ty, Op::Repeat(value, times)))
            }
            "as.Array" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("an `as.Array` of an unsupported form"));
                };
                // An Array holds no NA. A fresh vector from base R's constructor holds zeros,
                // empty strings or `FALSE`, so it converts without the check the interpreter runs.
                let a = self.value(a)?;
                let block = &self.builder.function.blocks[self.builder.current.0 as usize];
                let fresh = block
                    .insts
                    .iter()
                    .any(|inst| inst.dest == Some(a) && matches!(inst.op, Op::NewVector(..)));
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
                Ok(self
                    .builder
                    .emit(Type::vector(item), Op::NewVector(item, count)))
            }
            "List.sum" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a malformed `List.sum`"));
                };
                let mut nodes = SmallVec::new();
                self.arith_tree(a, &mut nodes)?;
                let (a, summed) = self.arith_nodes(&nodes, true)?;
                if summed {
                    if self.builder.ty(a) == Type::Dbl {
                        self.fail_if_nan(a);
                    }
                    return Ok(a);
                }
                match self.builder.ty(a) {
                    // The native sum gives up on an empty list, which the standard library
                    // sums to the R integer 0.
                    Type::Vector {
                        item: Item::I64,
                        array: true,
                    } => Ok(self
                        .builder
                        .emit(Type::I64, Op::Reduce(Reduce::Sum, Item::I64, a))),
                    Type::Vector {
                        item: Item::Dbl,
                        array: true,
                    } => {
                        let total = self
                            .builder
                            .emit(Type::Dbl, Op::Reduce(Reduce::Sum, Item::Dbl, a));
                        self.fail_if_nan(total);
                        Ok(total)
                    }
                    _ => Err(Refusal(
                        "a `List.sum` of something other than a number list",
                    )),
                }
            }
            "sum" | "prod" | "mean" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("a reduction of several arguments"));
                };
                let a = self.value(a)?;
                let reduce = match name {
                    "sum" => Reduce::Sum,
                    "prod" => Reduce::Prod,
                    _ => Reduce::Mean,
                };
                self.reduce(reduce, a)
            }
            "max" | "min" => {
                let operands = arguments.exactly(arguments.items.len())?;
                let reduce = if name == "max" {
                    Reduce::Max
                } else {
                    Reduce::Min
                };
                match operands[..] {
                    [a] => {
                        let a = self.value(a)?;
                        self.reduce(reduce, a)
                    }
                    [a, b] => {
                        let a = self.value(a)?;
                        let b = self.value(b)?;
                        if self.builder.ty(a).items().is_some()
                            || self.builder.ty(b).items().is_some()
                        {
                            return Err(Refusal("a `max` of vectors"));
                        }
                        let op = if name == "max" {
                            Arith::Max
                        } else {
                            Arith::Min
                        };
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
                let reduce = if name == "any" {
                    Reduce::Any
                } else {
                    Reduce::All
                };
                match self.builder.ty(a) {
                    Type::Lgl => Ok(a),
                    Type::Vector {
                        item: Item::Lgl, ..
                    } => Ok(self
                        .builder
                        .emit(Type::Lgl, Op::Reduce(reduce, Item::Lgl, a))),
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
                let value = arguments
                    .get("x", 0)
                    .ok_or(Refusal("a `round` without a value"))?;
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
                            Ok(self
                                .builder
                                .emit(self.builder.ty(value), Op::VectorRound(value, digits)))
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
                let value = arguments
                    .get("x", 0)
                    .ok_or(Refusal("a `formatC` without a value"))?;
                let value = self.value(value)?;
                if self.builder.ty(value) != Type::Int {
                    return Err(Refusal("a `formatC` of something other than an integer"));
                }
                let width = match arguments.named("width") {
                    Some(width) => {
                        constant_integer(width).ok_or(Refusal("a computed `formatC` width"))?
                    }
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
                Ok(self
                    .builder
                    .emit(Type::Str, Op::FormatPadded(value, width, zero)))
            }
            "sprintf" => {
                let [format, value] = arguments.exactly(2)?[..] else {
                    return Err(Refusal("a `sprintf` of an unsupported form"));
                };
                let format =
                    constant_string(format).ok_or(Refusal("a computed `sprintf` format"))?;
                let value = self.value(value)?;
                let ty = self.builder.ty(value);
                match (format.as_str(), ty) {
                    ("%d", Type::Int) | ("%s", Type::Int | Type::Dbl | Type::Str) => {
                        self.as_string(value)
                    }
                    (format, Type::Dbl | Type::Int) => {
                        let decimals = format
                            .strip_prefix("%.")
                            .and_then(|rest| rest.strip_suffix('f'))
                            .and_then(|digits| digits.parse::<i32>().ok())
                            .filter(|&digits| (0..=15).contains(&digits))
                            .ok_or(Refusal("a `sprintf` format the typed core does not know"))?;
                        let value = self.as_double(value)?;
                        Ok(self
                            .builder
                            .emit(Type::Str, Op::FormatFixed(value, decimals)))
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
                let result_item = if text == Text::Nchar {
                    Item::Int
                } else {
                    Item::Str
                };
                match self.builder.ty(a) {
                    Type::Str => Ok(self
                        .builder
                        .emit(Type::of_item(result_item), Op::Text(text, a))),
                    Type::Vector {
                        item: Item::Str, ..
                    } => Ok(self
                        .builder
                        .emit(Type::vector(result_item), Op::VectorText(text, a))),
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
                    Type::Vector {
                        item: Item::Str, ..
                    } => Ok(self
                        .builder
                        .emit(Type::vector(Item::Lgl), Op::VectorAffix(starts, a, affix))),
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
                let item = ty
                    .items()
                    .ok_or(Refusal("an index into something other than a vector"))?;
                if name == "[" && matches!(ty, Type::List(_)) {
                    return Err(Refusal("a `[` of a list"));
                }
                Ok(self
                    .builder
                    .emit(Type::of_item(item), Op::Get(vector, index)))
            }
            // A typed list carries its item type, so the runtime's empty-list storage fix is moot.
            ".ry_items" => {
                let [list, _] = arguments.exactly(2)?[..] else {
                    return Err(Refusal("a malformed `.ry_items`"));
                };
                self.value(list)
            }
            "lapply" | "vapply" | "List.map" => self.map(name, &arguments),
            "unlist" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal("an `unlist` of an unsupported form"));
                };
                let a = self.value(a)?;
                match self.builder.ty(a) {
                    Type::List(item) => Ok(self
                        .builder
                        .emit(Type::vector(item), Op::Relabel(a, Type::vector(item)))),
                    Type::Vector { .. } => Ok(a),
                    _ => Err(Refusal(
                        "an `unlist` of something other than a list of scalars",
                    )),
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
        let (vector, function, template) =
            match (name, &arguments.exactly(arguments.items.len())?[..]) {
                ("vapply", &[vector, function, template]) => (vector, function, Some(template)),
                ("lapply" | "List.map", &[vector, function]) => (vector, function, None),
                _ => return Err(Refusal("a map of an unsupported form")),
            };
        let (formals, body, scope) = self.function_argument(function)?;
        let vector = self.value(vector)?;
        let ty = self.builder.ty(vector);
        let item = ty
            .items()
            .ok_or(Refusal("a map over something that is not a vector"))?;
        if name == "vapply" && item == Item::Str {
            // `vapply` names its result by a character input, which the typed vector cannot hold.
            return Err(Refusal("a `vapply` over strings"));
        }
        let declared = match template {
            Some(template) => Some(
                template_item(template)
                    .ok_or(Refusal("a `vapply` template the typed core does not know"))?,
            ),
            None => None,
        };
        // `List.map` over a ry list builds a ry list of the results, which the interpreter
        // cannot do from no items at all.
        let to_array = name == "List.map" && matches!(ty, Type::Vector { array: true, .. });
        let held = self.builder.emit(ty, Op::Copy(vector));
        let count = self.builder.emit(Type::Int, Op::Length(held));
        if to_array {
            let zero = self.int(0);
            let empty = self.builder.emit(Type::Bool, Op::Same(count, zero));
            self.fail_if(empty);
        }
        let preheader = self.builder.current;
        // The result's type is known only once the body is lowered, so the output vector is
        // allocated in the preheader afterwards.
        let output = self.builder.local(Type::Null);
        let counter = self.builder.local(Type::Int);
        let one = self.int(1);
        self.builder.push(Some(counter), Op::Copy(one));
        let header = self.loop_header();
        let past = self.builder.emit(
            Type::Lgl,
            Op::Compare(Compare::Gt, Item::Int, counter, count),
        );
        let past = self.builder.emit(Type::Bool, Op::Truth(past));
        let (body_block, exit) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: past,
            then: exit,
            otherwise: body_block,
        });
        self.builder.switch_to(body_block);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::Get(held, counter));
        let value = self
            .inline(formals, body, scope, &[element])?
            .ok_or(Refusal("a mapped function that never completes"))?;
        let value_item = self
            .builder
            .ty(value)
            .item()
            .ok_or(Refusal("a mapped function whose value is not a scalar"))?;
        let result_item = declared.unwrap_or(value_item);
        let value = self.convert_scalar(value, result_item)?;
        let output_ty = if name == "vapply" {
            Type::vector(result_item)
        } else if to_array {
            if !matches!(result_item, Item::Dbl | Item::I64) {
                return Err(Refusal(
                    "a `List.map` to a list of something other than numbers",
                ));
            }
            Type::Vector {
                item: result_item,
                array: true,
            }
        } else {
            Type::List(result_item)
        };
        self.builder.function.locals[output.0 as usize] = output_ty;
        let updated = self
            .builder
            .emit(output_ty, Op::Set(output, counter, value));
        self.builder.push(Some(output), Op::Copy(updated));
        let next = self
            .builder
            .emit(Type::Int, Op::Arith(Arith::Add, Item::Int, counter, one));
        self.builder.push(Some(counter), Op::Copy(next));
        self.builder.terminate(Terminator::Jump(header));
        // The output is allocated before the loop, now that its type is known.
        self.builder.switch_to(preheader);
        let fresh = self
            .builder
            .emit(Type::vector(result_item), Op::NewVector(result_item, count));
        let fresh = if name == "vapply" {
            fresh
        } else {
            self.builder.emit(output_ty, Op::Relabel(fresh, output_ty))
        };
        self.builder.push(Some(output), Op::Copy(fresh));
        self.builder.switch_to(exit);
        Ok(output)
    }

    /// `List.count_if(list, predicate)` as an `I64` count, or `List.walk(list, state, f)` as the
    /// state each item's step leaves, in a loop over the items with the function inlined.
    unsafe fn walk(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let counting = name == "List.count_if";
        let (list, state, function) =
            match (counting, &arguments.exactly(arguments.items.len())?[..]) {
                (true, &[list, predicate]) => (list, None, predicate),
                (false, &[list, state, f]) => (list, Some(state), f),
                _ => return Err(Refusal("a walk of an unsupported form")),
            };
        let list = self.value(list)?;
        let ty = self.builder.ty(list);
        let item = match ty {
            Type::Vector { item, array: true } | Type::List(item) => item,
            _ => return Err(Refusal("a walk over something that is not a list")),
        };
        let state = match state {
            Some(state) => self.value(state)?,
            None => self.builder.emit(Type::I64, Op::Const(Constant::I64(0))),
        };
        let state_ty = self.builder.ty(state);
        let (formals, body, scope) = self.function_argument(function)?;
        let held = self.builder.emit(ty, Op::Copy(list));
        let count = self.builder.emit(Type::Int, Op::Length(held));
        let total = self.builder.emit(state_ty, Op::Copy(state));
        let counter = self.builder.local(Type::Int);
        let one = self.int(1);
        self.builder.push(Some(counter), Op::Copy(one));
        let header = self.loop_header();
        let past = self.builder.emit(
            Type::Lgl,
            Op::Compare(Compare::Gt, Item::Int, counter, count),
        );
        let past = self.builder.emit(Type::Bool, Op::Truth(past));
        let (body_block, exit) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: past,
            then: exit,
            otherwise: body_block,
        });
        self.builder.switch_to(body_block);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::Get(held, counter));
        if counting {
            let answer = self
                .inline(formals, body, scope, &[element])?
                .ok_or(Refusal("a predicate that never completes"))?;
            // The interpreter raises the error for an answer other than `TRUE` or `FALSE`.
            let answer = match self.builder.ty(answer) {
                Type::Bool => answer,
                Type::Lgl => self.builder.emit(Type::Bool, Op::Truth(answer)),
                _ => return Err(Refusal("a predicate whose answer is not a logical")),
            };
            let (add, after) = (self.builder.block(), self.builder.block());
            self.builder.terminate(Terminator::Branch {
                condition: answer,
                then: add,
                otherwise: after,
            });
            self.builder.switch_to(add);
            let unit = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
            let next = self
                .builder
                .emit(Type::I64, Op::Arith(Arith::Add, Item::I64, total, unit));
            self.builder.push(Some(total), Op::Copy(next));
            self.builder.terminate(Terminator::Jump(after));
            self.builder.switch_to(after);
        } else {
            let step = self
                .inline(formals, body, scope, &[total, element])?
                .ok_or(Refusal("a walk step that never completes"))?;
            if self.builder.ty(step) != state_ty {
                return Err(Refusal("a walk whose state changes type"));
            }
            self.builder.push(Some(total), Op::Copy(step));
        }
        let next = self
            .builder
            .emit(Type::Int, Op::Arith(Arith::Add, Item::Int, counter, one));
        self.builder.push(Some(counter), Op::Copy(next));
        self.builder.terminate(Terminator::Jump(header));
        self.builder.switch_to(exit);
        Ok(total)
    }

    /// The formals and body of a function argument: a literal or a local function.
    unsafe fn function_argument(&mut self, function: Sexp) -> Result<(Sexp, Sexp, usize), Refusal> {
        if function.kind() == SexpType::Symbol {
            return match self.lookup(function) {
                Some(Binding::Function(formals, body, scope)) => Ok((formals, body, scope)),
                _ => Err(Refusal("a mapped function that is not a local function")),
            };
        }
        if function.kind() == SexpType::Language
            && function.car().kind() == SexpType::Symbol
            && rvalues::symbol_name(function.car()) == "function"
        {
            let parts = rvalues::pairlist_items(function.cdr());
            if let (Some(&(_, formals)), Some(&(_, body))) = (parts.first(), parts.get(1)) {
                return Ok((formals, body, self.scopes.len() - 1));
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
            (false, Some(separator)) => {
                constant_string(separator).ok_or(Refusal("a computed `paste` separator"))?
            }
        };
        let collapse = match arguments.named("collapse") {
            Some(collapse) => {
                Some(constant_string(collapse).ok_or(Refusal("a computed `paste` collapse"))?)
            }
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
            self.builder.emit(
                Type::vector(Item::Str),
                Op::PasteVectors(vector_parts, separator),
            )
        } else if collapse.is_some() {
            let mut vector_parts = SmallVec::new();
            for part in parts {
                vector_parts.push(self.as_vector(part)?);
            }
            self.builder.emit(
                Type::vector(Item::Str),
                Op::PasteVectors(vector_parts, separator),
            )
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
            Type::List(item) => Ok(self
                .builder
                .emit(Type::vector(item), Op::Relabel(value, Type::vector(item)))),
            _ => {
                let item = ty.item().ok_or(Refusal("a value that is not a scalar"))?;
                let value = self.convert_scalar(value, item)?;
                let one = self.int(1);
                Ok(self
                    .builder
                    .emit(Type::vector(item), Op::Repeat(value, one)))
            }
        }
    }

    /// A number, string or vector of them as strings, as `paste` and `as.character` give them.
    unsafe fn as_string(&mut self, value: Local) -> Result<Local, Refusal> {
        match self.builder.ty(value) {
            Type::Str
            | Type::Vector {
                item: Item::Str, ..
            } => Ok(value),
            Type::Int | Type::Dbl => Ok(self.builder.emit(Type::Str, Op::Format(value))),
            Type::Vector {
                item: Item::Int | Item::Dbl,
                ..
            } => Ok(self
                .builder
                .emit(Type::vector(Item::Str), Op::Coerce(value, Item::Str))),
            _ => Err(Refusal("a value `paste` cannot turn into a string here")),
        }
    }

    unsafe fn as_double(&mut self, value: Local) -> Result<Local, Refusal> {
        match self.builder.ty(value) {
            Type::Dbl
            | Type::Vector {
                item: Item::Dbl, ..
            } => Ok(value),
            Type::Int | Type::Lgl | Type::Bool => {
                Ok(self.builder.emit(Type::Dbl, Op::ToDbl(value)))
            }
            Type::Vector {
                item: Item::Int | Item::Lgl,
                ..
            } => Ok(self
                .builder
                .emit(Type::vector(Item::Dbl), Op::Coerce(value, Item::Dbl))),
            _ => Err(Refusal("a value that is not a number")),
        }
    }

    unsafe fn reduce(&mut self, reduce: Reduce, value: Local) -> Result<Local, Refusal> {
        let ty = self.builder.ty(value);
        match ty {
            Type::Int | Type::Dbl
                if matches!(
                    reduce,
                    Reduce::Sum | Reduce::Prod | Reduce::Max | Reduce::Min
                ) =>
            {
                if reduce == Reduce::Prod && ty == Type::Int {
                    return Ok(self.builder.emit(Type::Dbl, Op::ToDbl(value)));
                }
                Ok(value)
            }
            Type::Lgl if reduce == Reduce::Sum => {
                Ok(self.builder.emit(Type::Int, Op::LglToInt(value)))
            }
            Type::Vector { item, .. } => {
                let (item, result) = match (item, reduce) {
                    (Item::Str, _) => return Err(Refusal("a reduction of strings")),
                    (_, Reduce::Mean | Reduce::Prod) => (Item::Dbl, Type::Dbl),
                    (Item::Dbl, _) => (Item::Dbl, Type::Dbl),
                    (Item::Int | Item::Lgl, Reduce::Sum) => (item, Type::Int),
                    (Item::Int, Reduce::Max | Reduce::Min) => (Item::Int, Type::Int),
                    _ => return Err(Refusal("a reduction the typed core does not know")),
                };
                let value = if item == Item::Dbl {
                    self.as_double(value)?
                } else {
                    value
                };
                Ok(self.builder.emit(result, Op::Reduce(reduce, item, value)))
            }
            _ => Err(Refusal("a reduction of something other than numbers")),
        }
    }

    unsafe fn math(&mut self, op: Math, value: Local) -> Result<Local, Refusal> {
        let ty = self.builder.ty(value);
        let keeps_integers = matches!(op, Math::Neg | Math::Abs);
        match ty {
            Type::I64 if op == Math::Neg => {
                Ok(self.builder.emit(Type::I64, Op::Math(op, Item::I64, value)))
            }
            Type::Int if keeps_integers => {
                Ok(self.builder.emit(Type::Int, Op::Math(op, Item::Int, value)))
            }
            Type::Lgl if keeps_integers => {
                let value = self.builder.emit(Type::Int, Op::LglToInt(value));
                Ok(self.builder.emit(Type::Int, Op::Math(op, Item::Int, value)))
            }
            Type::Int | Type::Lgl | Type::Dbl => {
                let value = self.as_double(value)?;
                Ok(self.builder.emit(Type::Dbl, Op::Math(op, Item::Dbl, value)))
            }
            Type::Vector {
                item: Item::Int, ..
            } if keeps_integers => Ok(self.builder.emit(
                Type::vector(Item::Int),
                Op::VectorMath(op, Item::Int, value),
            )),
            Type::Vector {
                item: Item::Int | Item::Lgl | Item::Dbl,
                ..
            } => {
                let value = self.as_double(value)?;
                Ok(self.builder.emit(
                    Type::vector(Item::Dbl),
                    Op::VectorMath(op, Item::Dbl, value),
                ))
            }
            _ => Err(Refusal("a math function of something other than numbers")),
        }
    }

    /// `Num.min(a, b)` or `Num.max(a, b)`. With an `I64` both are `I64`s; two R integers stay
    /// integers, and otherwise both are doubles. The first wins a tie, and `NA` or `NaN` is an
    /// error the interpreter raises.
    unsafe fn extreme(&mut self, maximum: bool, a: Local, b: Local) -> Result<Local, Refusal> {
        let op = if maximum { Arith::Max } else { Arith::Min };
        match (self.builder.ty(a), self.builder.ty(b)) {
            (Type::I64, _) | (_, Type::I64) => self.arith(op, a, b),
            (Type::Int, Type::Int) => {
                // `ToI64` fails on `NA`, and the integers themselves are the operands.
                self.builder.emit(Type::I64, Op::ToI64(a));
                self.builder.emit(Type::I64, Op::ToI64(b));
                Ok(self.builder.emit(Type::Int, Op::Arith(op, Item::Int, a, b)))
            }
            (Type::Int | Type::Dbl, Type::Int | Type::Dbl) => {
                let a = self.as_double(a)?;
                let b = self.as_double(b)?;
                self.fail_if_nan(a);
                self.fail_if_nan(b);
                let comparison = if maximum { Compare::Gt } else { Compare::Lt };
                let second = self
                    .builder
                    .emit(Type::Lgl, Op::Compare(comparison, Item::Dbl, b, a));
                let second = self.builder.emit(Type::Bool, Op::Truth(second));
                let result = self.builder.local(Type::Dbl);
                let (take_second, take_first, join) = (
                    self.builder.block(),
                    self.builder.block(),
                    self.builder.block(),
                );
                self.builder.terminate(Terminator::Branch {
                    condition: second,
                    then: take_second,
                    otherwise: take_first,
                });
                self.builder.switch_to(take_second);
                self.builder.push(Some(result), Op::Copy(b));
                self.builder.terminate(Terminator::Jump(join));
                self.builder.switch_to(take_first);
                self.builder.push(Some(result), Op::Copy(a));
                self.builder.terminate(Terminator::Jump(join));
                self.builder.switch_to(join);
                Ok(result)
            }
            _ => Err(Refusal("a `Num.min` of something other than numbers")),
        }
    }

    unsafe fn arith(&mut self, op: Arith, a: Local, b: Local) -> Result<Local, Refusal> {
        let (ta, tb) = (self.builder.ty(a), self.builder.ty(b));
        let array_item = |ty: Type| match ty {
            Type::Vector { item, array: true } => Some(item),
            _ => None,
        };
        // Arithmetic on ry lists keeps the list. Its items never recycle and are never missing,
        // which the native helper checks; an `I64` list takes an exact R number as its scalar.
        if let Some(item) = array_item(ta).or(array_item(tb)) {
            let side = |lower: &mut Self, value: Local| -> Result<Local, Refusal> {
                match (lower.builder.ty(value), item) {
                    (
                        Type::Vector {
                            item: other,
                            array: true,
                        },
                        _,
                    ) if other == item => Ok(value),
                    (Type::Dbl, Item::Dbl) | (Type::I64, Item::I64) => Ok(value),
                    (Type::Dbl | Type::Int, Item::I64) => lower.to_i64(value),
                    _ => Err(Refusal("list arithmetic between different item types")),
                }
            };
            let valid = match item {
                Item::Dbl => true,
                Item::I64 => matches!(op, Arith::Add | Arith::Sub | Arith::Mul),
                _ => false,
            };
            if !valid {
                return Err(Refusal("list arithmetic the typed core does not compile"));
            }
            let a = side(self, a)?;
            let b = side(self, b)?;
            return Ok(self.builder.emit(
                Type::Vector { item, array: true },
                Op::VectorArith(op, item, a, b),
            ));
        }
        // An R number next to an `I64` converts exactly, and the result is an `I64`.
        if ta == Type::I64 || tb == Type::I64 {
            if op == Arith::Div {
                return Err(Refusal("a `/` of an I64, which is an error"));
            }
            let a = self.to_i64(a)?;
            let b = self.to_i64(b)?;
            return Ok(self.builder.emit(Type::I64, Op::Arith(op, Item::I64, a, b)));
        }
        let numeric_item = |ty: Type| match ty {
            Type::Int
            | Type::Lgl
            | Type::Bool
            | Type::Vector {
                item: Item::Int | Item::Lgl,
                ..
            } => Some(Item::Int),
            Type::Dbl
            | Type::Vector {
                item: Item::Dbl, ..
            } => Some(Item::Dbl),
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
        let vectors = ta.items().is_some() || tb.items().is_some();
        // When an integer or logical `NA` meets a `NaN`, whether the interpreter gives `NA` or
        // `NaN` depends on the operand order its compiled kernel picked for that operator
        // (`NaN + NA` is `NA` but `NaN - NA` is `NaN`), so a `NaN` next to an R integer fails
        // over to it.
        if !vectors && matches!(op, Arith::Add | Arith::Sub | Arith::Mul | Arith::Div) {
            match (ta, tb) {
                (Type::Dbl, Type::Int | Type::Lgl) => self.fail_if_nan(a),
                (Type::Int | Type::Lgl, Type::Dbl) => self.fail_if_nan(b),
                _ => {}
            }
        }
        let a = self.operand(a, item)?;
        let b = self.operand(b, item)?;
        if vectors {
            Ok(self
                .builder
                .emit(Type::vector(item), Op::VectorArith(op, item, a, b)))
        } else {
            Ok(self
                .builder
                .emit(Type::of_item(item), Op::Arith(op, item, a, b)))
        }
    }

    /// Appends the arithmetic expression tree of `expression` to `nodes`, children before their
    /// parent, and returns its root. Operands that are not base arithmetic are lowered as
    /// leaves, in R's evaluation order.
    unsafe fn arith_tree(
        &mut self,
        expression: Sexp,
        nodes: &mut SmallVec<[Node; 16]>,
    ) -> Result<u8, Refusal> {
        let node = match self.arith_call(expression)? {
            Some((op, a, b)) => {
                let left = self.arith_tree(a, nodes)?;
                let right = self.arith_tree(b, nodes)?;
                Node::Arith(op, left, right)
            }
            None => Node::Leaf(self.value(expression)?),
        };
        let index =
            u8::try_from(nodes.len()).map_err(|_| Refusal("an arithmetic expression too large"))?;
        nodes.push(node);
        Ok(index)
    }

    /// The operator and operands of a binary call of base R's arithmetic, looking through
    /// parentheses.
    unsafe fn arith_call(
        &mut self,
        expression: Sexp,
    ) -> Result<Option<(Arith, Sexp, Sexp)>, Refusal> {
        let mut expression = expression;
        loop {
            if expression.kind() != SexpType::Language
                || expression.car().kind() != SexpType::Symbol
            {
                return Ok(None);
            }
            let head = expression.car();
            let name = rvalues::symbol_name(head);
            let arguments = Arguments::of(expression);
            if name == "(" {
                let [inner] = arguments.exactly(1)?[..] else {
                    return Ok(None);
                };
                expression = inner;
                continue;
            }
            if !matches!(&*name, "+" | "-" | "*" | "/" | "^" | "%%" | "%/%")
                || arguments.items.len() != 2
                || arguments.items.iter().any(|(tag, _)| tag.is_some())
                || self.lookup(head).is_some()
            {
                return Ok(None);
            }
            let Some(speculation) = resolve_function(head, self.env) else {
                return Ok(None);
            };
            if !is_base_function(&speculation) {
                return Ok(None);
            }
            self.speculations.push(speculation);
            let op = arith_op(&name);
            return Ok(Some((op, arguments.items[0].1, arguments.items[1].1)));
        }
    }

    /// Emits an arithmetic tree. A tree of two or more list operations runs as one fused pass,
    /// which with `sum` also sums the items and then returns `true`; anything else emits
    /// operation by operation.
    unsafe fn arith_nodes(&mut self, nodes: &[Node], sum: bool) -> Result<(Local, bool), Refusal> {
        let array_item = |ty: Type| match ty {
            Type::Vector { item, array: true } => Some(item),
            _ => None,
        };
        // Each node's value once emitted, and whether its subtree reads a list.
        let mut values: SmallVec<[Option<Local>; 16]> = SmallVec::new();
        let mut lists: SmallVec<[bool; 16]> = SmallVec::new();
        let mut item = None;
        for &node in nodes {
            match node {
                Node::Leaf(local) => {
                    let found = array_item(self.builder.ty(local));
                    item = item.or(found);
                    values.push(Some(local));
                    lists.push(found.is_some());
                }
                Node::Arith(op, left, right) => {
                    let list = lists[left as usize] || lists[right as usize];
                    // A subtree of scalars is scalar arithmetic, done once.
                    let value = match (list, values[left as usize], values[right as usize]) {
                        (false, Some(a), Some(b)) => Some(self.arith(op, a, b)?),
                        _ => None,
                    };
                    values.push(value);
                    lists.push(list);
                }
            }
        }
        let root = nodes.len() - 1;
        let operations = lists
            .iter()
            .zip(nodes)
            .filter(|&(&list, node)| list && matches!(node, Node::Arith(..)))
            .count();
        if let Some(item) = item
            && (operations >= 2 || sum && operations >= 1)
            && let Some(fused) = self.fused(nodes, &values, &lists, item, sum)?
        {
            return Ok((fused, sum));
        }
        for (index, &node) in nodes.iter().enumerate() {
            if let (Node::Arith(op, left, right), None) = (node, values[index]) {
                let (Some(a), Some(b)) = (values[left as usize], values[right as usize]) else {
                    return Err(Refusal("an arithmetic tree out of order"));
                };
                values[index] = Some(self.arith(op, a, b)?);
            }
        }
        values[root]
            .map(|value| (value, false))
            .ok_or(Refusal("an empty arithmetic tree"))
    }

    /// The fused pass over the list operations of an arithmetic tree whose scalar subtrees are
    /// emitted, or `None` when the tree has an operation or an operand the pass does not take.
    unsafe fn fused(
        &mut self,
        nodes: &[Node],
        values: &[Option<Local>],
        lists: &[bool],
        item: Item,
        sum: bool,
    ) -> Result<Option<Local>, Refusal> {
        #[derive(Clone, Copy)]
        enum Operand {
            Input(u32),
            Step(u32),
        }
        let mut inputs: SmallVec<[Local; 4]> = SmallVec::new();
        let mut steps: SmallVec<[(Arith, Operand, Operand); 8]> = SmallVec::new();
        // The step each list operation became, by node.
        let mut stepped: SmallVec<[Option<u32>; 16]> = SmallVec::new();
        for (index, &node) in nodes.iter().enumerate() {
            stepped.push(None);
            let Node::Arith(op, left, right) = node else {
                continue;
            };
            if !lists[index] {
                continue;
            }
            let allowed = match item {
                Item::Dbl => !matches!(op, Arith::Min | Arith::Max),
                Item::I64 => matches!(op, Arith::Add | Arith::Sub | Arith::Mul),
                _ => false,
            };
            if !allowed {
                return Ok(None);
            }
            let mut pair = [Operand::Input(0); 2];
            for (slot, child) in pair.iter_mut().zip([left as usize, right as usize]) {
                if let Some(step) = stepped[child] {
                    *slot = Operand::Step(step);
                    continue;
                }
                let Some(value) = values[child] else {
                    return Ok(None);
                };
                let value = match (self.builder.ty(value), item) {
                    (
                        Type::Vector {
                            item: other,
                            array: true,
                        },
                        _,
                    ) if other == item => value,
                    (Type::Dbl, Item::Dbl) | (Type::I64, Item::I64) => value,
                    (Type::Dbl | Type::Int, Item::I64) => self.to_i64(value)?,
                    _ => return Ok(None),
                };
                let position = inputs
                    .iter()
                    .position(|&input| input == value)
                    .unwrap_or_else(|| {
                        inputs.push(value);
                        inputs.len() - 1
                    });
                *slot = Operand::Input(position as u32);
            }
            stepped[index] = Some(steps.len() as u32);
            steps.push((op, pair[0], pair[1]));
        }
        if inputs.len() > values::ELEMENTWISE_INPUTS || steps.len() > values::ELEMENTWISE_STEPS {
            return Ok(None);
        }
        let base = inputs.len() as u32;
        let number = |operand: Operand| match operand {
            Operand::Input(input) => input,
            Operand::Step(step) => base + step,
        };
        let program = steps
            .iter()
            .map(|&(op, left, right)| op as u32 | number(left) << 8 | number(right) << 16)
            .collect();
        let ty = match (sum, item) {
            (true, Item::Dbl) => Type::Dbl,
            (true, _) => Type::I64,
            (false, _) => Type::Vector { item, array: true },
        };
        Ok(Some(self.builder.emit(
            ty,
            Op::Elementwise {
                item,
                sum,
                program,
                inputs,
            },
        )))
    }

    /// A numeric operand converted to `item`, as a scalar or a vector as it was.
    unsafe fn operand(&mut self, value: Local, item: Item) -> Result<Local, Refusal> {
        let ty = self.builder.ty(value);
        match (ty, item) {
            (
                Type::Vector {
                    item: Item::Dbl, ..
                },
                Item::Dbl,
            )
            | (
                Type::Vector {
                    item: Item::Int, ..
                },
                Item::Int,
            ) => Ok(value),
            (Type::Vector { item: from, .. }, _) if from != item => Ok(self
                .builder
                .emit(Type::vector(item), Op::Coerce(value, item))),
            (Type::Bool, _) => {
                let lgl = self.builder.emit(Type::Lgl, Op::Copy(value));
                self.convert_scalar(lgl, item)
            }
            _ => self.convert_scalar(value, item),
        }
    }

    unsafe fn compare(&mut self, op: Compare, a: Local, b: Local) -> Result<Local, Refusal> {
        let (ta, tb) = (self.builder.ty(a), self.builder.ty(b));
        // A comparison with an `I64` converts the other side as arithmetic does, and its
        // logical is never `NA`.
        if ta == Type::I64 || tb == Type::I64 {
            let a = self.to_i64(a)?;
            let b = self.to_i64(b)?;
            return Ok(self
                .builder
                .emit(Type::Bool, Op::Compare(op, Item::I64, a, b)));
        }
        let vectors = ta.items().is_some() || tb.items().is_some();
        let item_of = |ty: Type| ty.item().or_else(|| ty.items());
        let (Some(ia), Some(ib)) = (item_of(ta), item_of(tb)) else {
            return Err(Refusal(
                "a comparison of something other than scalars or vectors",
            ));
        };
        let item = match (ia, ib) {
            (Item::Str, Item::Str) => {
                if !matches!(op, Compare::Eq | Compare::Ne) {
                    return Err(Refusal(
                        "an ordering comparison of strings, which depends on the locale",
                    ));
                }
                Item::Str
            }
            (Item::Str, _) | (_, Item::Str) => {
                return Err(Refusal("a comparison of a string and a number"));
            }
            (Item::Dbl, _) | (_, Item::Dbl) => Item::Dbl,
            _ => Item::Int,
        };
        let (a, b) = if item == Item::Str {
            (a, b)
        } else {
            (self.operand(a, item)?, self.operand(b, item)?)
        };
        if vectors {
            Ok(self
                .builder
                .emit(Type::vector(Item::Lgl), Op::VectorCompare(op, item, a, b)))
        } else {
            Ok(self.builder.emit(Type::Lgl, Op::Compare(op, item, a, b)))
        }
    }
}

/// The R storage type of a scalar or plain vector of `ty`, `None` for an Array, a list or a
/// value with no R storage.
fn plain_storage(ty: Type) -> Option<SexpType> {
    match crate::storage(ty)? {
        (kind, false) if !matches!(ty, Type::List(_)) => Some(kind),
        _ => None,
    }
}

/// A node of an arithmetic expression tree, in a list where children come before their parent.
#[derive(Clone, Copy)]
enum Node {
    Leaf(Local),
    Arith(Arith, u8, u8),
}

fn arith_op(name: &str) -> Arith {
    match name {
        "+" => Arith::Add,
        "-" => Arith::Sub,
        "*" => Arith::Mul,
        "/" => Arith::Div,
        "^" => Arith::Pow,
        "%%" => Arith::Mod,
        _ => Arith::IntDiv,
    }
}

/// How a counted loop steps from its start to its end.
#[derive(Clone, Copy)]
enum Step {
    /// `seq_len`, `seq_along` and a vector: up by this step from 1, empty when the end is 0.
    FromOne(Local),
    /// `a:b`: up or down by one toward the end, which it always reaches.
    Signed,
    /// `List.range(a, b)`: up by this step, an `I64`, empty when the end is below the start.
    Inclusive(Local),
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
    if value.kind() != SexpType::String
        || rvalues::length(value) != 1
        || value.attrib() != rvalues::nil()
    {
        return None;
    }
    rvalues::string_at(value, 0).map(|text| text.into_owned())
}

/// Every symbol an assignment in `expression` writes, `for` variables included, outside nested
/// function literals.
/// Whether `expression`, local functions included, assigns `symbol` with `<-`, `=`, `<<-` or
/// as a `for` variable.
unsafe fn assigns(expression: Sexp, symbol: Sexp) -> bool {
    if expression.kind() != SexpType::Language {
        return false;
    }
    let head = expression.car();
    let items = rvalues::pairlist_items(expression.cdr());
    if head.kind() == SexpType::Symbol
        && matches!(&*rvalues::symbol_name(head), "<-" | "=" | "<<-" | "for")
        && let Some(&(_, mut target)) = items.first()
    {
        while target.kind() == SexpType::Language {
            match rvalues::pairlist_items(target.cdr()).first() {
                Some(&(_, inner)) => target = inner,
                None => break,
            }
        }
        if target == symbol {
            return true;
        }
    }
    assigns(head, symbol) || items.iter().any(|&(_, item)| assigns(item, symbol))
}

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
        // An `if` without `else` whose condition is false gives an invisible `NULL`.
        "if" => {
            args.len() < 3
                || args
                    .iter()
                    .skip(1)
                    .any(|&(_, branch)| tail_invisible(branch))
        }
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
            heap.frame_cell(heap.known().base_namespace, speculation.symbol)
                == Some(speculation.cell)
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

/// Walks from `start` to the binding a variable lookup of `symbol` finds, giving up at a promise
/// or an active binding. The speculation's `function` is the bound value.
pub unsafe fn resolve_value(symbol: Sexp, start: Sexp) -> Option<Speculation> {
    let heap = interp::heap();
    let mut env = start;
    let empty = heap.known().empty_env;
    while env != empty {
        if let Some(cell) = heap.frame_cell(env, symbol) {
            let value = cell.cdr();
            if cell.has_gp(heap::object::gp::ACTIVE_BINDING) || value.kind() == SexpType::Promise {
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
