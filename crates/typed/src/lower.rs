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
            Arith, Bits, BlockId, Builder, Compare, Constant, Item, Local, Math, Op, Reduce,
            Terminator, Text, Type,
        },
        shapes, values,
    },
    heap::{
        object::{Sexp, SexpType},
        records::{Fields, Layout},
    },
    interp::values as rvalues,
    rustc_hash::FxHashMap,
    smallvec::SmallVec,
    std::{borrow::Cow, cell::Cell},
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
    /// The R calls the code makes, as [`Op::CallR`] indexes them.
    pub r_calls: Vec<RCall>,
    /// The [`shapes::epoch`] whose confirmed shapes a `match` over any tag tests for.
    pub epoch: Option<u64>,
    /// The tag shapes the code constructs, which the caller confirms once the code compiles.
    pub constructed: SmallVec<[u32; 4]>,
}

/// A call of an R function from typed code, which compiled code makes through
/// [`crate::ry_call_r`]. The function sees what it would see in the interpreter: each argument
/// is a promise of the code written in the call, already forced, so `substitute()` gives that
/// code, and `sys.call()` and its errors give the call as written.
pub struct RCall {
    /// The function the call resolved to when the code compiled, which a speculation keeps
    /// current.
    pub function: Sexp,
    pub call: Sexp,
    /// The typed closure's environment, which the conversions run in.
    pub env: Sexp,
    pub arguments: SmallVec<[RArgument; 4]>,
    /// The `Num.from_r`, `Str.from_r`, `Bool.from_r` or `List.from_r` call the result goes
    /// through and the type it gives, or `None` when the call is a statement whose value is
    /// unused.
    pub result: Option<(Conversion, Type)>,
    /// Whether the call is `stop()`, which raises an error and never returns, so it cannot
    /// change a variable a lazy argument reads before R would force it.
    pub diverges: bool,
}

/// An argument of an [`RCall`]: a native value of `ty`, passed through `conversion` when the
/// code converts it with `Num.to_r` or `List.to_r`.
pub struct RArgument {
    pub tag: Sexp,
    pub code: Sexp,
    pub ty: Type,
    pub conversion: Option<Conversion>,
}

/// A standard library conversion between ry and R values, applied as the interpreter applies it,
/// so it raises the same errors.
#[derive(Clone, Copy)]
pub struct Conversion {
    pub call: Sexp,
    pub function: Sexp,
}

/// How a refusal that waits on the interpreter starts. The type a conversion of what an R call
/// returns gives depends on the value, which the native tier learns from a run of the conversion
/// in the interpreter, and a call of `pkg::name` resolves only once running the code has loaded
/// the namespace. The closure compiles again once the interpreter has done what the refusal
/// waits on.
pub const AWAITING: &str = "it waits on the interpreter to run `";

/// Whether `refusal` waits on the interpreter, so the closure compiles again once it has run.
pub fn awaits_interpreter(refusal: &Refusal) -> bool {
    refusal.0.starts_with(AWAITING)
}

/// What a refusal that waits on the interpreter waits for.
#[derive(Clone, Copy)]
pub enum Awaited {
    /// A run of this `Num.from_r` or `List.from_r` call.
    Conversion(Sexp),
    /// The loading of the namespace of this package name.
    Namespace(Sexp),
    /// The forcing of this lazy-load promise, which a namespace binds a function to until the
    /// first use reads it from the package database.
    Promise(Sexp),
}

/// Whether the interpreter has done what `awaited` waits for.
pub unsafe fn arrived(awaited: Awaited) -> bool {
    match awaited {
        Awaited::Conversion(call) => interp::numbers::observed_conversion(call).is_some(),
        Awaited::Namespace(package) => loaded_namespace(package).is_some(),
        Awaited::Promise(promise) => promise.car() != rvalues::unbound(),
    }
}

thread_local! {
    /// What the last refusal that waits on the interpreter waits for.
    static AWAITED: Cell<Option<Awaited>> = const { Cell::new(None) };
}

/// What the last refusal that waits on the interpreter waits for.
pub fn awaited() -> Option<Awaited> {
    AWAITED.get()
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
    declared: Option<Type>,
    callees: &mut dyn Callees,
) -> Result<Lowered, Refusal> {
    let formals = rvalues::pairlist_items(closure.car());
    if formals.len() != params.len() {
        return Err(Refusal::new(
            "the call does not supply every formal positionally",
        ));
    }
    let body = interp::bytecode::closure_body(closure);
    if tail_invisible(body) {
        return Err(Refusal::new("the body's value may be invisible"));
    }
    let name = String::from("typed");
    let mut lowering = Lowering {
        builder: Builder::new(name, params),
        closure,
        env: closure.tag(),
        params: SmallVec::from_slice(params),
        guess,
        declared,
        tails: SmallVec::new(),
        callees,
        scopes: vec![Scope::default()],
        loops: Vec::new(),
        inlined: Vec::new(),
        speculations: Vec::new(),
        nesting: 0,
        control: 0,
        unusable: SmallVec::new(),
        epoch: None,
        returned: None,
        constructed: SmallVec::new(),
        r_calls: Vec::new(),
        r_use: None,
        converted: None,
        pending: Vec::new(),
    };
    tail_matches(body, &mut lowering.tails);
    let mut assigned = Vec::new();
    assigned_symbols(body, &mut assigned);
    for (index, &(symbol, _)) in formals.iter().enumerate() {
        if symbol == rvalues::symbols().dots {
            return Err(Refusal::new("a function with `...`"));
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
        let ty = lowering.builder.ty(value);
        let result = match lowering.returned {
            Some(returned) => {
                joined(returned, ty).ok_or(Refusal::new("a `return` of another type"))?
            }
            None => ty,
        };
        lowering.builder.function.result = result;
        lowering.builder.terminate(Terminator::Return(value));
    } else {
        lowering.builder.function.result = lowering.returned.unwrap_or(guess);
    }
    Ok(Lowered {
        function: lowering.builder.function,
        speculations: lowering.speculations,
        r_calls: lowering.r_calls,
        epoch: lowering.epoch,
        constructed: lowering.constructed,
    })
}

/// A function passed to another: inlined where it is called, or a typed closure called directly.
#[derive(Clone, Copy, Debug)]
enum Callable {
    Inline(Sexp, Sexp, usize),
    Typed(Sexp),
}

/// What a name in scope stands for.
#[derive(Clone, Copy, Debug)]
enum Binding {
    Value(Local),
    /// A local function: its formals, its body and the index of the scope that defines it, whose
    /// names are the only ones the body sees when it is inlined.
    Function(Sexp, Sexp, usize),
}

#[derive(Clone, Default)]
struct Scope {
    names: FxHashMap<Sexp, Binding>,
}

/// An inlined function's exit: the block a `return` jumps to and the local it writes.
#[derive(Clone, Copy)]
struct Inlined {
    exit: BlockId,
    result: Option<Local>,
}

/// What [`Lowering::rewind`] restores.
struct Checkpoint {
    scopes: Vec<Scope>,
    loops: usize,
    inlined: Vec<Inlined>,
    speculations: usize,
    nesting: usize,
    control: usize,
    unusable: usize,
    returned: Option<Type>,
    constructed: usize,
    r_calls: usize,
    blocks: usize,
}

struct Lowering<'c> {
    builder: Builder,
    closure: Sexp,
    env: Sexp,
    params: SmallVec<[Type; 4]>,
    guess: Type,
    /// The result type the function declares, which a `match` whose value is the function's
    /// result lowers its arms against ([`Lowering::switch_shapes`]).
    declared: Option<Type>,
    /// The `match` calls whose value is the function's result.
    tails: SmallVec<[Sexp; 2]>,
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
    /// The earliest [`shapes::epoch`] a `match` over any tag relied on.
    epoch: Option<u64>,
    /// The type an early `return` of the function itself gives, the union of the tags when it
    /// returns tags.
    returned: Option<Type>,
    /// The tag shapes the code constructs, which a `match` over any tag also tests for.
    constructed: SmallVec<[u32; 4]>,
    r_calls: Vec<RCall>,
    /// How the value of the call lowered next is used, when that call may be an R call: as a
    /// statement whose value is unused, or through a conversion. Every call takes it.
    r_use: Option<RUse>,
    /// The destination of the last converted R call, which tells the conversion that its
    /// argument was one.
    converted: Option<Local>,
    /// Each empty list literal of records or tags whose item type the code has not shown yet,
    /// with the locals that hold it. The preflight names only the storage of such a list, so it
    /// starts as a list of any tag and takes the item type of the first item appended to it or
    /// list assigned over it ([`Lowering::settle`]).
    pending: Vec<SmallVec<[Local; 4]>>,
}

/// Where typed code may call an R function: where the value is unused, where a conversion
/// gives it a ry type, or as `stop()`, which never returns.
#[derive(Clone, Copy)]
enum RUse {
    Unused,
    Converted(Conversion),
    Diverges,
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
            return Err(Refusal::new("a call with unexpected arguments"));
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
            return Err(Refusal::new("expressions nest too deeply"));
        }
        let value = self.expression_inner(expression);
        self.nesting -= 1;
        value
    }

    /// Lowers `expression` that must complete.
    unsafe fn value(&mut self, expression: Sexp) -> Result<Local, Refusal> {
        let value = self
            .expression(expression)?
            .ok_or(Refusal::new("an argument that never completes"))?;
        self.usable(value)?;
        Ok(value)
    }

    fn usable(&self, value: Local) -> Result<(), Refusal> {
        if self.unusable.contains(&value) {
            return Err(Refusal::new(
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
            _ => Err(Refusal::new("an expression of an unsupported kind")),
        }
    }

    unsafe fn constant(&mut self, value: Sexp) -> Result<Local, Refusal> {
        if value.kind() == SexpType::Nil {
            return Ok(self.builder.emit(Type::Null, Op::Const(Constant::Null)));
        }
        if value.attrib() != rvalues::nil() || rvalues::length(value) != 1 {
            return Err(Refusal::new("a constant that is not a plain scalar"));
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
                let text =
                    rvalues::string_at(value, 0).ok_or(Refusal::new("an NA string constant"))?;
                return Ok(self.string(&text));
            }
            _ => return Err(Refusal::new("a constant of an unsupported type")),
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
            Some(Binding::Function(..)) => Err(Refusal::new("a local function used as a value")),
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
            return Err(Refusal(Cow::Owned(format!(
                "it reads `{}`, which is neither one of its locals nor a standard library \
                 constant: pass the value in as a parameter",
                rvalues::symbol_name(symbol)
            ))));
        };
        // Locals are bound as lowering reaches their assignment, so a read that a loop runs
        // again after a later assignment of the same name would keep reading the constant.
        if assigns(interp::bytecode::closure_body(self.closure), symbol) {
            return Err(Refusal::new("a library constant the function assigns"));
        }
        let local = self.constant(speculation.function)?;
        self.speculations.push(speculation);
        Ok(local)
    }

    unsafe fn assign(&mut self, symbol: Sexp, value: Local) -> Result<Local, Refusal> {
        match self.lookup_assignable(symbol) {
            Some(variable) => {
                self.unify_pending(variable, value)?;
                let ty = self.builder.ty(value);
                let fits = match (self.builder.ty(variable), ty) {
                    (Type::Tag(held), Type::Tag(found)) => shapes::within(found, held),
                    (held, found) => held == found,
                };
                if !fits {
                    return Err(Refusal::new("a variable that changes type"));
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
                    return Err(Refusal::new("a local function rebound in a branch or loop"));
                }
                let variable = self.builder.emit(self.builder.ty(value), Op::Copy(value));
                if let Some(family) = self.pending_family(value) {
                    self.pending[family].push(variable);
                }
                self.bind(symbol, Binding::Value(variable));
                Ok(variable)
            }
        }
    }

    /// The index of the pending empty list `local` holds, if it holds one.
    fn pending_family(&self, local: Local) -> Option<usize> {
        self.pending
            .iter()
            .position(|family| family.contains(&local))
    }

    /// Refuses to read the items of a pending empty list, whose item type is a placeholder.
    fn known_items(&self, list: Local) -> Result<(), Refusal> {
        match self.pending_family(list) {
            Some(_) => Err(Refusal::new(
                "an empty list read before anything shows its item type",
            )),
            None => Ok(()),
        }
    }

    /// Gives a pending empty list the item type of the list it meets in an assignment, or joins
    /// two pending ones.
    fn unify_pending(&mut self, a: Local, b: Local) -> Result<(), Refusal> {
        match (self.pending_family(a), self.pending_family(b)) {
            (Some(first), Some(second)) if first != second => {
                let merged = self.pending.swap_remove(first.max(second));
                self.pending[first.min(second)].extend(merged);
                Ok(())
            }
            (Some(_), Some(_)) | (None, None) => Ok(()),
            (Some(_), None) => match self.builder.ty(b) {
                Type::Vector { item, array: true } => self.settle(a, item),
                _ => Ok(()),
            },
            (None, Some(_)) => match self.builder.ty(a) {
                Type::Vector { item, array: true } => self.settle(b, item),
                _ => Ok(()),
            },
        }
    }

    /// Makes the pending empty list `list` holds a list of `item`, retyping every local that
    /// holds it and the instruction that allocated it. Only copies and lengths may have read it
    /// before, since any other operation chose its code by the placeholder item type.
    fn settle(&mut self, list: Local, item: Item) -> Result<(), Refusal> {
        let Some(family) = self.pending_family(list) else {
            return Ok(());
        };
        let members = self.pending.swap_remove(family);
        let function = &mut self.builder.function;
        for inst in function
            .blocks
            .iter_mut()
            .flat_map(|block| &mut block.insts)
        {
            let reads = inst
                .op
                .operands()
                .iter()
                .any(|operand| members.contains(operand));
            if reads && !matches!(inst.op, Op::Copy(_) | Op::Length(_) | Op::Relabel(..)) {
                return Err(Refusal::new(
                    "an empty list read before anything shows its item type",
                ));
            }
            if inst.dest.is_some_and(|dest| members.contains(&dest)) {
                match &mut inst.op {
                    Op::NewVector(held, _) | Op::EmptyList(held, _) => *held = item,
                    Op::Relabel(_, Type::Vector { item: held, .. }) => *held = item,
                    _ => {}
                }
            }
        }
        for member in members {
            if let Some(Type::Vector { item: held, .. }) =
                function.locals.get_mut(member.0 as usize)
            {
                *held = item;
            }
        }
        Ok(())
    }

    /// A list literal `[a, b, ...]` (`.ry_array`), or an empty one whose storage the preflight
    /// named (`hint`), with room for `capacity` items when it comes from `List.with_capacity`.
    /// An empty list of records or tags is pending until its item type shows.
    unsafe fn list_literal(
        &mut self,
        arguments: &Arguments,
        hint: Option<&str>,
        capacity: Option<Local>,
    ) -> Result<Local, Refusal> {
        let mut values: SmallVec<[Local; 4]> = SmallVec::new();
        for &(ref name, item) in &arguments.items {
            if name.is_some() {
                return Err(Refusal::new("a list literal with a named item"));
            }
            let value = self.value(item)?;
            self.usable(value)?;
            values.push(value);
        }
        let item = match values.first() {
            Some(&first) => {
                let ty = self.builder.ty(first);
                let item = ty.held_item().ok_or(Refusal::new(
                    "a list literal of something a list cannot hold",
                ))?;
                if values
                    .iter()
                    .any(|&value| self.builder.ty(value).held_item() != Some(item))
                {
                    return Err(Refusal::new("a list literal whose items differ in type"));
                }
                item
            }
            None => match hint {
                None => Item::Lgl,
                Some("I64") => Item::I64,
                Some("F64") => Item::Dbl,
                Some("Str") => Item::Str,
                Some("list") => Item::Tag(shapes::ANY),
                Some(_) => return Err(Refusal::new("an empty list of an unknown item type")),
            },
        };
        let fresh = match capacity {
            Some(capacity) => self
                .builder
                .emit(Type::vector(item), Op::EmptyList(item, capacity)),
            None => {
                let zero = self.int(0);
                self.builder
                    .emit(Type::vector(item), Op::NewVector(item, zero))
            }
        };
        let ty = Type::Vector { item, array: true };
        let mut list = self.builder.emit(ty, Op::Relabel(fresh, ty));
        if values.is_empty() && hint == Some("list") {
            self.pending.push(SmallVec::from_slice(&[fresh, list]));
        }
        for value in values {
            list = self.builder.emit(ty, Op::Push(list, value));
        }
        Ok(list)
    }

    /// `List.append(list, item)`, which grows a uniquely held list in place.
    unsafe fn append(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, item] = arguments.exactly(2)?[..] else {
            return Err(Refusal::new("a malformed `List.append`"));
        };
        let list = self.value(list)?;
        let value = self.value(item)?;
        self.usable(list)?;
        self.usable(value)?;
        let item = self
            .builder
            .ty(value)
            .held_item()
            .ok_or(Refusal::new("an append of something a list cannot hold"))?;
        self.settle(list, item)?;
        let ty = self.builder.ty(list);
        let fits = match (ty, item) {
            (
                Type::Vector {
                    item: Item::Tag(held),
                    array: true,
                },
                Item::Tag(found),
            ) => shapes::within(found, held),
            (
                Type::Vector {
                    item: held,
                    array: true,
                },
                found,
            ) => held == found,
            _ => return Err(Refusal::new("an append to something other than a ry list")),
        };
        if !fits {
            return Err(Refusal::new("an append of an item of another type"));
        }
        Ok(self.builder.emit(ty, Op::Push(list, value)))
    }

    /// A ry list argument whose item type is known, for a list function that reads or moves its
    /// items.
    unsafe fn known_list(&mut self, list: Sexp, function: &str) -> Result<(Local, Type), Refusal> {
        let list = self.value(list)?;
        self.usable(list)?;
        let ty = self.builder.ty(list);
        if !matches!(ty, Type::Vector { array: true, .. }) {
            return Err(Refusal(Cow::Owned(format!(
                "a `{function}` of something other than a ry list"
            ))));
        }
        self.known_items(list)?;
        Ok((list, ty))
    }

    /// `List.concat(first, second)`. An empty list on either side takes the other's item type.
    unsafe fn concat(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [first, second] = arguments.exactly(2)?[..] else {
            return Err(Refusal::new("a malformed `List.concat`"));
        };
        let first = self.value(first)?;
        let second = self.value(second)?;
        self.usable(first)?;
        self.usable(second)?;
        let item = |ty: Type| match ty {
            Type::Vector { item, array: true } => Some(item),
            _ => None,
        };
        let (Some(first_item), Some(second_item)) =
            (item(self.builder.ty(first)), item(self.builder.ty(second)))
        else {
            return Err(Refusal::new(
                "a `List.concat` of something other than ry lists",
            ));
        };
        match (self.pending_family(first), self.pending_family(second)) {
            (Some(_), None) => self.settle(first, second_item)?,
            (None, Some(_)) => self.settle(second, first_item)?,
            _ => {}
        }
        self.known_items(first)?;
        self.known_items(second)?;
        let ty = self.builder.ty(first);
        if ty != self.builder.ty(second) {
            return Err(Refusal::new(
                "a `List.concat` of lists of different item types",
            ));
        }
        Ok(self.builder.emit(ty, Op::Extend(first, second)))
    }

    /// `List.swap(list, first, second)`.
    unsafe fn swap(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, first, second] = arguments.exactly(3)?[..] else {
            return Err(Refusal::new("a malformed `List.swap`"));
        };
        let (list, ty) = self.known_list(list, "List.swap")?;
        let first = self.value(first)?;
        let first = self.to_i64(first)?;
        let second = self.value(second)?;
        let second = self.to_i64(second)?;
        Ok(self.builder.emit(ty, Op::Swap(list, first, second)))
    }

    /// `List.sublist`, `List.take_first`, `List.take_last`, `List.drop_first` and
    /// `List.drop_last` as one slice. Counts clamp to the list; a negative count, or a start
    /// before 1, leaves the call to the interpreter, which raises the library's error.
    unsafe fn slice(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let sublist = name == "List.sublist";
        let parameters = arguments.exactly(if sublist { 3 } else { 2 })?;
        let (list, ty) = self.known_list(parameters[0], name)?;
        let mut numbers: SmallVec<[Local; 2]> = SmallVec::new();
        for &parameter in &parameters[1..] {
            let value = self.value(parameter)?;
            numbers.push(self.to_i64(value)?);
        }
        let total = self.builder.emit(Type::I64, Op::Length(list));
        let constant = |lower: &mut Self, value: i64| {
            lower
                .builder
                .emit(Type::I64, Op::Const(Constant::I64(value)))
        };
        let arith = |lower: &mut Self, op: Arith, a: Local, b: Local| {
            lower
                .builder
                .emit(Type::I64, Op::Arith(op, Item::I64, a, b))
        };
        let zero = constant(self, 0);
        // A negative count stays negative through `Min`, and the slice then fails.
        let (start, count) = if sublist {
            let one = constant(self, 1);
            let before = self.builder.emit(
                Type::Lgl,
                Op::Compare(Compare::Lt, Item::I64, numbers[0], one),
            );
            self.fail_if(before);
            let start = arith(self, Arith::Sub, numbers[0], one);
            let start = arith(self, Arith::Min, start, total);
            let rest = arith(self, Arith::Sub, total, start);
            (start, arith(self, Arith::Min, numbers[1], rest))
        } else {
            let negative = self.builder.emit(
                Type::Lgl,
                Op::Compare(Compare::Lt, Item::I64, numbers[0], zero),
            );
            self.fail_if(negative);
            let clamped = arith(self, Arith::Min, numbers[0], total);
            let rest = arith(self, Arith::Sub, total, clamped);
            match name {
                "List.take_first" => (zero, clamped),
                "List.take_last" => (rest, clamped),
                "List.drop_first" => (clamped, rest),
                _ => (zero, rest),
            }
        };
        Ok(self.builder.emit(ty, Op::Slice(list, start, count)))
    }

    /// The variable an assignment in the current scope writes, if it exists there.
    fn lookup_assignable(&self, symbol: Sexp) -> Option<Local> {
        match self.scopes.last()?.names.get(&symbol) {
            Some(Binding::Value(local)) if (local.0 as usize) >= self.params.len() => Some(*local),
            _ => None,
        }
    }

    unsafe fn call(&mut self, call: Sexp) -> Result<Option<Local>, Refusal> {
        let r_use = self.r_use.take();
        let head = call.car();
        if head.kind() != SexpType::Symbol {
            if let Some(speculation) = namespaced_function(head) {
                let Some(r_use) = r_use else {
                    return Err(r_value_refusal(head));
                };
                let speculation = speculation?;
                self.speculations.push(speculation);
                return self.r_call(call, speculation.function, r_use).map(Some);
            }
            return Err(Refusal::new("a call of a computed function"));
        }
        let name = rvalues::symbol_name(head).into_owned();
        // The language's own forms, unless the program rebinds them.
        match name.as_str() {
            "{" => return self.sequence(call),
            "(" => {
                let [inner] = self.arguments(call, 1)?[..] else {
                    return Err(Refusal::new("a malformed parenthesis"));
                };
                return self.expression(inner);
            }
            "<-" | "=" => return self.assignment(call).map(Some),
            "if" => return self.conditional(call),
            "for" => return self.for_loop(call),
            "while" => return self.while_loop(call),
            "repeat" => return self.repeat_loop(call),
            "break" | "next" => {
                let &(continue_to, break_to) = self
                    .loops
                    .last()
                    .ok_or(Refusal::new("`break` outside a loop"))?;
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
                    _ => return Err(Refusal::new("`return` with several values")),
                };
                self.exit(value)?;
                return Ok(None);
            }
            "function" => return Err(Refusal::new("a function literal that escapes")),
            "<<-" => return Err(Refusal::new("a superassignment")),
            "stop" => {
                // R raises the error, with its message and the typed function's call, so code
                // that has called R, and cannot run again, reports it as the interpreter would.
                // A message typed code cannot pass to R gives up instead, and the interpreter
                // raises it.
                let raise = self.builder.block();
                self.builder.terminate(Terminator::Jump(raise));
                self.builder.switch_to(raise);
                let checkpoint = self.checkpoint();
                let raised = match resolve_function(head, self.env) {
                    Some(speculation) => {
                        let function = speculation.function;
                        self.speculations.push(speculation);
                        self.r_call(call, function, RUse::Diverges)
                    }
                    None => Err(Refusal::new("`stop` is not a function")),
                };
                if raised.is_ok() {
                    self.builder.terminate(Terminator::Fail);
                } else if !self.rewind(checkpoint, raise) {
                    return raised.map(|_| None);
                }
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
            return Err(Refusal::new("a call of a local variable"));
        }
        let Some(speculation) = resolve_function(head, self.env) else {
            return Err(Refusal(Cow::Owned(format!(
                "it calls `{name}`, which no binding defines as a function"
            ))));
        };
        let function = speculation.function;
        if is_base_function(&speculation) {
            self.speculations.push(speculation);
            if name == ".ry_match" {
                return self.match_expression(call);
            }
            self.r_use = r_use;
            return self.builtin(&name, call, function).map(Some);
        }
        if function.kind() == SexpType::Closure && interp::annotations::is_typed(function) {
            self.speculations.push(speculation);
            let arguments = Arguments::of(call);
            if arguments.items.iter().any(|(tag, _)| tag.is_some()) {
                return Err(Refusal::new("a typed call with named arguments"));
            }
            let values = self.call_arguments(&arguments)?;
            return self.typed_call(function, &values).map(Some);
        }
        if let Some(r_use) = r_use {
            self.speculations.push(speculation);
            return self.r_call(call, function, r_use).map(Some);
        }
        Err(r_value_refusal(head))
    }

    /// A call of the R function `function`, whose value is unused or converted as `r_use` says.
    unsafe fn r_call(&mut self, call: Sexp, function: Sexp, r_use: RUse) -> Result<Local, Refusal> {
        let name = || interp::deparse::deparse_first_line(call.car());
        let result = match r_use {
            RUse::Unused | RUse::Diverges => None,
            RUse::Converted(conversion) => Some((conversion, conversion_type(conversion)?)),
        };
        let diverges = matches!(r_use, RUse::Diverges);
        let calls = self.r_calls.len();
        let mut arguments: SmallVec<[RArgument; 4]> = SmallVec::new();
        let mut values: SmallVec<[Local; 4]> = SmallVec::new();
        for (tag, code) in rvalues::pairlist_items(call.cdr()) {
            if code == rvalues::symbols().dots || code == rvalues::missing_arg() {
                return Err(Refusal(Cow::Owned(format!(
                    "it calls R's `{}` with `...` or an empty argument",
                    name()
                ))));
            }
            let (value, conversion) = match self.to_r_conversion(code) {
                Some((inner, conversion)) => (self.value(inner)?, Some(conversion)),
                None => (self.value(code)?, None),
            };
            let ty = self.builder.ty(value);
            let passes = match (conversion.is_some(), ty) {
                (_, Type::Set(_) | Type::Dict { .. }) => false,
                (
                    false,
                    Type::I64
                    | Type::Vector {
                        item: Item::I64, ..
                    },
                ) => false,
                (true, ty) => {
                    matches!(ty, Type::I64 | Type::Dbl | Type::Vector { array: true, .. })
                }
                (false, _) => true,
            };
            if !passes {
                return Err(Refusal(Cow::Owned(format!(
                    "it passes a value of type {ty:?} to R's `{}`, which takes an `I64` only \
                     through `Num.to_r` and a list of them only through `List.to_r`",
                    name()
                ))));
            }
            arguments.push(RArgument {
                tag,
                code,
                ty,
                conversion,
            });
            values.push(value);
        }
        // R evaluates arguments lazily, in the order the function asks for them, and compiled
        // code computes them first, which only agrees while computing them calls no R function.
        if self.r_calls.len() != calls {
            return Err(Refusal(Cow::Owned(format!(
                "it calls R inside the arguments of R's `{}`, whose order R's lazy arguments \
                 decide: compute the inner call into a variable first",
                name()
            ))));
        }
        self.r_calls.push(RCall {
            function,
            call,
            env: self.env,
            arguments,
            result,
            diverges,
        });
        let site = self.r_calls.len() - 1;
        Ok(match result {
            None => {
                self.builder.push(None, Op::CallR(site, values));
                self.builder.emit(Type::Null, Op::Const(Constant::Null))
            }
            Some((_, ty)) => {
                let dest = self.builder.emit(ty, Op::CallR(site, values));
                self.converted = Some(dest);
                dest
            }
        })
    }

    /// `Num.to_r(value)` or `List.to_r(value)`, as the value and the conversion.
    unsafe fn to_r_conversion(&mut self, code: Sexp) -> Option<(Sexp, Conversion)> {
        if code.kind() != SexpType::Language || code.car().kind() != SexpType::Symbol {
            return None;
        }
        let name = rvalues::symbol_name(code.car());
        if name != "Num.to_r" && name != "List.to_r" || self.lookup(code.car()).is_some() {
            return None;
        }
        let [(tag, inner)] = rvalues::pairlist_items(code.cdr())[..] else {
            return None;
        };
        if tag != rvalues::nil() {
            return None;
        }
        let speculation = resolve_function(code.car(), self.env)?;
        if !is_base_function(&speculation) {
            return None;
        }
        self.speculations.push(speculation);
        Some((
            inner,
            Conversion {
                call: code,
                function: speculation.function,
            },
        ))
    }

    /// A direct call of the compiled typed closure `function`.
    unsafe fn typed_call(&mut self, function: Sexp, values: &[Local]) -> Result<Local, Refusal> {
        let types: SmallVec<[Type; 4]> =
            values.iter().map(|&value| self.builder.ty(value)).collect();
        let (index, result) = if function == self.closure && types[..] == self.params[..] {
            (SELF, self.guess)
        } else {
            self.callees.callee(function, &types)?
        };
        Ok(self
            .builder
            .emit(result, Op::CallTyped(index, SmallVec::from_slice(values))))
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
                        let (expected, found) = (self.builder.ty(result), self.builder.ty(value));
                        if expected != found && (expected, found) != (Type::Lgl, Type::Bool) {
                            return Err(Refusal::new("a `return` of another type"));
                        }
                        self.builder.push(Some(result), Op::Copy(value));
                    }
                    None => return Err(Refusal::new("a `return` before the result type is known")),
                }
                self.builder.terminate(Terminator::Jump(exit));
            }
            None => {
                let value = if interp::annotations::returns_i64(self.closure)
                    && matches!(self.builder.ty(value), Type::Int | Type::Dbl)
                {
                    self.to_i64(value)?
                } else {
                    value
                };
                // A recursive call assumes the guess, and the driver lowers again until the
                // result, which includes every early `return`, matches it.
                let ty = self.builder.ty(value);
                self.returned = Some(match self.returned {
                    Some(returned) => {
                        joined(returned, ty).ok_or(Refusal::new("a `return` of another type"))?
                    }
                    None => ty,
                });
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
        let count = items.len();
        for (position, (_, item)) in items.into_iter().enumerate() {
            // A statement's value is unused, so it may be a call of an R function.
            if position + 1 < count {
                self.r_use = Some(RUse::Unused);
            }
            let value = self.expression(item);
            self.r_use = None;
            last = value?;
            if last.is_none() {
                return Ok(None);
            }
        }
        Ok(last)
    }

    unsafe fn assignment(&mut self, call: Sexp) -> Result<Local, Refusal> {
        let [target, value] = self.arguments(call, 2)?[..] else {
            return Err(Refusal::new("a malformed assignment"));
        };
        if target.kind() == SexpType::Symbol {
            if value.kind() == SexpType::Language
                && value.car().kind() == SexpType::Symbol
                && rvalues::symbol_name(value.car()) == "function"
                && self.lookup(value.car()).is_none()
            {
                let parts = rvalues::pairlist_items(value.cdr());
                let (Some(&(_, formals)), Some(&(_, body))) = (parts.first(), parts.get(1)) else {
                    return Err(Refusal::new("a malformed function literal"));
                };
                if self.control > 0 {
                    return Err(Refusal::new("a local function defined in a branch or loop"));
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
                    return Err(Refusal::new("an indexed assignment of an unsupported form"));
                };
                if vector.kind() != SexpType::Symbol {
                    return Err(Refusal::new("an indexed assignment into an expression"));
                }
                let variable = self.lookup_assignable(vector).ok_or(Refusal::new(
                    "an indexed assignment into a parameter or a non-local",
                ))?;
                let index = self.value(index)?;
                let index = self.integer_index(index)?;
                let value = self.value(value)?;
                let items = self
                    .builder
                    .ty(variable)
                    .items()
                    .ok_or(Refusal::new("an indexed assignment into a non-vector"))?;
                let value = self.convert_scalar(value, items)?;
                let updated = self
                    .builder
                    .emit(self.builder.ty(variable), Op::Set(variable, index, value));
                self.builder.push(Some(variable), Op::Copy(updated));
                return Ok(value);
            }
        }
        Err(Refusal::new("an assignment of an unsupported form"))
    }

    unsafe fn integer_index(&mut self, index: Local) -> Result<Local, Refusal> {
        match self.builder.ty(index) {
            Type::Int | Type::I64 => Ok(index),
            // A whole double indexes as that integer. A fraction gives up, so the interpreter
            // truncates it as R does.
            Type::Dbl => self.to_i64(index),
            _ => Err(Refusal::new("an index that is not an integer")),
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
            (Type::Record(a), Item::Record(b)) if a == b => Ok(value),
            (Type::Tag(a), Item::Tag(b)) if shapes::within(a, b) => Ok(value),
            (Type::Vector { item, array: true }, Item::Array(id))
                if shapes::array_of(item) == id =>
            {
                Ok(value)
            }
            (Type::Int | Type::Lgl, Item::Dbl) => {
                Ok(self.builder.emit(Type::Dbl, Op::ToDbl(value)))
            }
            (Type::Lgl, Item::Int) => Ok(self.builder.emit(Type::Int, Op::LglToInt(value))),
            _ => Err(Refusal::new("an item of another type")),
        }
    }

    unsafe fn condition(&mut self, condition: Sexp) -> Result<Local, Refusal> {
        let value = self.value(condition)?;
        match self.builder.ty(value) {
            Type::Lgl => Ok(self.builder.emit(Type::Bool, Op::Truth(value))),
            Type::Bool => Ok(value),
            _ => Err(Refusal::new("a condition that is not a logical")),
        }
    }

    unsafe fn conditional(&mut self, call: Sexp) -> Result<Option<Local>, Refusal> {
        let items = rvalues::pairlist_items(call.cdr());
        let (condition, then, otherwise) = match items[..] {
            [(_, condition), (_, then)] => (condition, then, None),
            [(_, condition), (_, then), (_, otherwise)] => (condition, then, Some(otherwise)),
            _ => return Err(Refusal::new("a malformed `if`")),
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
                    let Some(ty) = joined(self.builder.ty(then_local), self.builder.ty(else_local))
                    else {
                        continue;
                    };
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
                // A branch whose value nothing may read passes that on to the `if`'s value.
                if self.unusable.contains(&value) {
                    self.unusable.push(result);
                }
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
                // Two nested `if`s whose own branches disagree both hold a placeholder `NULL`,
                // so equal types alone do not make the joined value readable.
                let readable = !self.unusable.contains(&a) && !self.unusable.contains(&b);
                let ty = joined(self.builder.ty(a), self.builder.ty(b)).filter(|_| readable);
                if let Some(ty) = ty {
                    let result = self.builder.local(ty);
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
            return Err(Refusal::new("a malformed `for`"));
        };
        // A counted loop over `seq_len(n)`, `seq_along(x)`, `a:b` or `List.range(a, b)`
        // allocates nothing.
        let (start, end, step, items) = match self.counted_range(sequence)? {
            Some((start, end, step)) => (start, end, step, None),
            None => {
                let vector = self.value(sequence)?;
                self.known_items(vector)?;
                let ty = self.builder.ty(vector);
                ty.items()
                    .ok_or(Refusal::new("a `for` over something that is not a vector"))?;
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
                        .ok_or(Refusal::new("a loop over a non-vector"))?,
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
                    return Err(Refusal::new("a malformed sequence"));
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
                    return Err(Refusal::new("a malformed `List.range`"));
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
                    return Err(Refusal::new("a malformed `:`"));
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
                    .ok_or(Refusal::new("a `:` of computed doubles"))?;
                if constant.fract() != 0.0 || constant.abs() > 1e9 {
                    return Err(Refusal::new("a `:` of fractional doubles"));
                }
                Ok(self.int(constant as i32))
            }
            _ => Err(Refusal::new("a `:` of something other than numbers")),
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
            _ => Err(Refusal::new("an I64 from something other than a number")),
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
                    .ok_or(Refusal::new("a count that is a computed double"))?;
                if constant.fract() != 0.0 || !(0.0..=2e9).contains(&constant) {
                    return Err(Refusal::new("a count that is not a whole number"));
                }
                Ok(self.int(constant as i32))
            }
            Type::I64 => Ok(self.builder.emit(Type::Int, Op::CountOfI64(value))),
            _ => Err(Refusal::new("a count that is not a number")),
        }
    }

    unsafe fn while_loop(&mut self, call: Sexp) -> Result<Option<Local>, Refusal> {
        let [condition, body] = self.arguments(call, 2)?[..] else {
            return Err(Refusal::new("a malformed `while`"));
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
            return Err(Refusal::new("a malformed `repeat`"));
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
            return Err(Refusal::new("local functions inline too deeply"));
        }
        let definition = formals;
        let formals = rvalues::pairlist_items(formals);
        if formals.len() != arguments.len() {
            return Err(Refusal::new(
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
                return Err(Refusal::new("a local function with `...`"));
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
            Err(Refusal::new("a `return` inside a local function"))
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
                let kinds: SmallVec<[Option<(SexpType, bool)>; 4]> = arguments
                    .iter()
                    .map(|&argument| inline_storage(self.builder.ty(argument)))
                    .collect();
                let result_kind = inline_storage(self.builder.ty(value));
                if !interp::annotations::admits_inline(definition, body, &kinds, result_kind) {
                    return Err(Refusal::new(
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
    unsafe fn builtin(&mut self, name: &str, call: Sexp, function: Sexp) -> Result<Local, Refusal> {
        // Only this call may be an R call; the calls in its arguments are used as values.
        let r_use = self.r_use.take();
        let arguments = Arguments::of(call);
        match name {
            "+" | "-" | "*" | "/" | "^" | "%%" | "%/%" => {
                let operands: SmallVec<[Sexp; 4]> = arguments.positional().collect();
                if arguments.items.len() != operands.len() {
                    return Err(Refusal::new("arithmetic with named arguments"));
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
                            Err(Refusal::new("a unary `+` of a non-number"))
                        }
                    }
                    [a, b] => {
                        let mut nodes = SmallVec::new();
                        let left = self.arith_tree(a, &mut nodes)?;
                        let right = self.arith_tree(b, &mut nodes)?;
                        nodes.push(Node::Arith(arith_op(name), left, right));
                        self.arith_nodes(&nodes, false).map(|(value, _)| value)
                    }
                    _ => Err(Refusal::new(
                        "arithmetic with an unexpected number of operands",
                    )),
                }
            }
            "==" | "!=" | "<" | "<=" | ">" | ">=" => {
                let [a, b] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed comparison"));
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
                    return Err(Refusal::new("a malformed `!`"));
                };
                let a = self.value(a)?;
                match self.builder.ty(a) {
                    Type::Lgl | Type::Bool => Ok(self.builder.emit(Type::Lgl, Op::Not(a))),
                    // `!` of a ry list is a ry list.
                    ty @ Type::Vector {
                        item: Item::Lgl, ..
                    } => Ok(self.builder.emit(ty, Op::VectorNot(a))),
                    _ => Err(Refusal::new("a `!` of something other than logicals")),
                }
            }
            "&" | "|" => {
                let [a, b] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed logical operator"));
                };
                let a = self.value(a)?;
                let b = self.value(b)?;
                let and = name == "&";
                let (ta, tb) = (self.builder.ty(a), self.builder.ty(b));
                let list = |ty: Type| matches!(ty, Type::Vector { array: true, .. });
                let plain = |ty: Type| matches!(ty, Type::Vector { array: false, .. });
                if (list(ta) || list(tb)) && (plain(ta) || plain(tb)) {
                    return Err(Refusal::new("a logical operator on a list and an R vector"));
                }
                let result = Type::Vector {
                    item: Item::Lgl,
                    array: list(ta) || list(tb),
                };
                match (ta, tb) {
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
                        Ok(self.builder.emit(result, Op::VectorLogic(and, a, b)))
                    }
                    _ => Err(Refusal::new(
                        "a logical operator on something other than logicals",
                    )),
                }
            }
            "&&" | "||" => {
                let [a, b] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed `&&`"));
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
            "Set.from_list" => {
                let [list] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `Set.from_list`"));
                };
                let list = self.value(list)?;
                match self.builder.ty(list) {
                    Type::Vector {
                        item: item @ (Item::I64 | Item::Str),
                        array: true,
                    } => Ok(self.builder.emit(Type::Set(item), Op::Unique(list))),
                    _ => Err(Refusal::new(
                        "a `Set.from_list` of something other than a list of integers or strings",
                    )),
                }
            }
            "Set.to_list" | "Set.len" => {
                let [set] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed set function"));
                };
                let set = self.value(set)?;
                let Type::Set(item) = self.builder.ty(set) else {
                    return Err(Refusal::new("a set function of something other than a set"));
                };
                if name == "Set.len" {
                    return Ok(self.builder.emit(Type::I64, Op::Length(set)));
                }
                // The set's items are already a list in first-insertion order.
                let list = Type::Vector { item, array: true };
                Ok(self.builder.emit(list, Op::Relabel(set, list)))
            }
            "List.sort_asc" | "List.sort_desc" => {
                let [list] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed sort"));
                };
                let list = self.value(list)?;
                let ty = self.builder.ty(list);
                match ty {
                    Type::Vector {
                        item: Item::I64 | Item::Dbl,
                        array: true,
                    } => Ok(self
                        .builder
                        .emit(ty, Op::Sort(list, name == "List.sort_desc"))),
                    _ => Err(Refusal::new(
                        "a sort of something other than a list of numbers",
                    )),
                }
            }
            "List.len" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `List.len`"));
                };
                let a = self.value(a)?;
                match self.builder.ty(a) {
                    Type::Vector { array: true, .. } | Type::List(_) => {
                        Ok(self.builder.emit(Type::I64, Op::Length(a)))
                    }
                    _ => Err(Refusal::new("a `List.len` of something other than a list")),
                }
            }
            "List.range" => {
                let [from, to] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed `List.range`"));
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
                    return Err(Refusal::new("a malformed `Num.to_f64`"));
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
                    _ => Err(Refusal::new(
                        "a `Num.to_f64` of something other than a number",
                    )),
                }
            }
            "Num.min" | "Num.max" => {
                let [a, b] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed `Num.min`"));
                };
                let a = self.value(a)?;
                let b = self.value(b)?;
                let maximum = name == "Num.max";
                self.extreme(maximum, a, b)
            }
            "Num.abs" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `Num.abs`"));
                };
                let a = self.value(a)?;
                match self.builder.ty(a) {
                    Type::I64 => Ok(self
                        .builder
                        .emit(Type::I64, Op::Math(Math::Abs, Item::I64, a))),
                    Type::Dbl => {
                        // The library refuses `NaN` and `NA`.
                        self.fail_if_nan(a);
                        Ok(self
                            .builder
                            .emit(Type::Dbl, Op::Math(Math::Abs, Item::Dbl, a)))
                    }
                    _ => Err(Refusal::new(
                        "a `Num.abs` of something other than an I64 or F64",
                    )),
                }
            }
            "Num.pow_int" => {
                let [base, exponent] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed `Num.pow_int`"));
                };
                let base = self.value(base)?;
                let exponent = self.value(exponent)?;
                if self.builder.ty(base) != Type::I64 && self.builder.ty(exponent) != Type::I64 {
                    return Err(Refusal::new("a `Num.pow_int` of R numbers"));
                }
                self.arith(Arith::Pow, base, exponent)
            }
            "Num.bitwise_and"
            | "Num.bitwise_or"
            | "Num.bitwise_xor"
            | "Num.shift_left_by"
            | "Num.shift_right_by"
            | "Num.shift_right_zf_by" => {
                let [a, b] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed bit operation"));
                };
                let a = self.value(a)?;
                let b = self.value(b)?;
                // On R numbers the library works on 32 bits, which the typed core does not.
                if self.builder.ty(a) != Type::I64 && self.builder.ty(b) != Type::I64 {
                    return Err(Refusal::new("a bit operation on R numbers"));
                }
                let a = self.to_i64(a)?;
                let b = self.to_i64(b)?;
                let op = match name {
                    "Num.bitwise_and" => Bits::And,
                    "Num.bitwise_or" => Bits::Or,
                    "Num.bitwise_xor" => Bits::Xor,
                    "Num.shift_left_by" => Bits::ShiftLeft,
                    "Num.shift_right_by" => Bits::ShiftRight,
                    _ => Bits::ShiftRightZf,
                };
                Ok(self.builder.emit(Type::I64, Op::Bits(op, a, b)))
            }
            "Num.bitwise_not" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `Num.bitwise_not`"));
                };
                let a = self.value(a)?;
                if self.builder.ty(a) != Type::I64 {
                    return Err(Refusal::new("a bit operation on R numbers"));
                }
                let ones = self.builder.emit(Type::I64, Op::Const(Constant::I64(-1)));
                Ok(self.builder.emit(Type::I64, Op::Bits(Bits::Xor, a, ones)))
            }
            "Num.compare" => {
                let [a, b] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed `Num.compare`"));
                };
                let a = self.value(a)?;
                let b = self.value(b)?;
                self.compare_numbers(a, b)
            }
            "Num.is_nan" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `Num.is_nan`"));
                };
                let a = self.value(a)?;
                if self.builder.ty(a) != Type::Dbl {
                    return Err(Refusal::new(
                        "a `Num.is_nan` of something other than an F64",
                    ));
                }
                Ok(self.builder.emit(Type::Bool, Op::IsNan(a)))
            }
            "Num.sqrt" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `Num.sqrt`"));
                };
                let a = self.value(a)?;
                let a = match self.builder.ty(a) {
                    Type::Int | Type::Dbl => self.as_double(a)?,
                    _ => return Err(Refusal::new("a `Num.sqrt` of something other than an F64")),
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
                    return Err(Refusal::new("a malformed `Num.pow`"));
                };
                let base = self.value(base)?;
                let exponent = self.value(exponent)?;
                let mut doubles = [base, exponent];
                for value in &mut doubles {
                    *value = match self.builder.ty(*value) {
                        Type::Int | Type::Dbl => self.as_double(*value)?,
                        _ => return Err(Refusal::new("a `Num.pow` of something other than F64s")),
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
                    return Err(Refusal::new("a malformed `Num.round`"));
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
                    _ => Err(Refusal::new("a `Num.round` of something other than an F64")),
                }
            }
            "List.count_if" | "List.walk" => self.walk(name, &arguments),
            "length" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `length`"));
                };
                let a = self.value(a)?;
                Ok(self.length(a))
            }
            "seq_len" => {
                let [n] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `seq_len`"));
                };
                let n = self.value(n)?;
                let n = self.integer_count(n)?;
                Ok(self.builder.emit(Type::vector(Item::Int), Op::SeqLen(n)))
            }
            "seq_along" => {
                let [x] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `seq_along`"));
                };
                let x = self.value(x)?;
                let n = self.length(x);
                Ok(self.builder.emit(Type::vector(Item::Int), Op::SeqLen(n)))
            }
            ":" => {
                let [from, to] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed `:`"));
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
                    return Err(Refusal::new("a `rep` of an unsupported form"));
                }
                let value = arguments
                    .get("x", 0)
                    .or_else(|| arguments.named("value"))
                    .ok_or(Refusal::new("a `rep` without a value"))?;
                let times = arguments
                    .get("times", 1)
                    .or_else(|| arguments.named("count"))
                    .or_else(|| arguments.positional().nth(1))
                    .ok_or(Refusal::new("a `rep` without a count"))?;
                let value = self.value(value)?;
                let times = self.value(times)?;
                let times = self.integer_count(times)?;
                let item = self
                    .builder
                    .ty(value)
                    .item()
                    .ok_or(Refusal::new("a `rep` of a vector"))?;
                let value = self.convert_scalar(value, item)?;
                // `List.repeat` builds an Array, as the standard library does, except of R
                // integers, which no Array holds.
                let ty = match (name, item) {
                    ("List.repeat", Item::Int) => Type::List(item),
                    ("List.repeat", _) => Type::Vector { item, array: true },
                    _ => Type::vector(item),
                };
                Ok(self.builder.emit(ty, Op::Repeat(value, times)))
            }
            "as.Array" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("an `as.Array` of an unsupported form"));
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
                    _ => Err(Refusal::new("an `as.Array` of a vector that may hold NA")),
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
                    _ => return Err(Refusal::new("a vector constructor of an unsupported form")),
                };
                Ok(self
                    .builder
                    .emit(Type::vector(item), Op::NewVector(item, count)))
            }
            "List.sum" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `List.sum`"));
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
                    _ => Err(Refusal::new(
                        "a `List.sum` of something other than a number list",
                    )),
                }
            }
            "sum" | "prod" | "mean" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a reduction of several arguments"));
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
                            return Err(Refusal::new("a `max` of vectors"));
                        }
                        let op = if name == "max" {
                            Arith::Max
                        } else {
                            Arith::Min
                        };
                        self.arith(op, a, b)
                    }
                    _ => Err(Refusal::new("a `max` of an unsupported form")),
                }
            }
            "any" | "all" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `any`"));
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
                    _ => Err(Refusal::new("an `any` of something other than logicals")),
                }
            }
            "abs" | "sqrt" | "exp" | "log" | "floor" | "ceiling" | "trunc" | "sin" | "cos"
            | "tan" | "log2" | "log10" | "sign" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a math function of several arguments"));
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
                    .ok_or(Refusal::new("a `round` without a value"))?;
                let value = self.value(value)?;
                let value = self.as_double(value)?;
                match arguments.get("digits", 1) {
                    None if arguments.items.len() == 1 => self.math(Math::Round, value),
                    Some(digits) if arguments.items.len() == 2 => {
                        let digits = self.value(digits)?;
                        let digits = match self.builder.ty(digits) {
                            Type::Int => self.builder.emit(Type::Dbl, Op::ToDbl(digits)),
                            Type::Dbl => digits,
                            _ => {
                                return Err(Refusal::new(
                                    "a `round` to digits that are not a number",
                                ));
                            }
                        };
                        if self.builder.ty(value).items().is_some() {
                            Ok(self
                                .builder
                                .emit(self.builder.ty(value), Op::VectorRound(value, digits)))
                        } else {
                            Ok(self.builder.emit(Type::Dbl, Op::RoundDigits(value, digits)))
                        }
                    }
                    _ => Err(Refusal::new("a `round` of an unsupported form")),
                }
            }
            "as.double" | "as.numeric" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `as.double`"));
                };
                let a = self.value(a)?;
                self.as_double(a)
            }
            "as.character" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `as.character`"));
                };
                let a = self.value(a)?;
                self.as_string(a)
            }
            "paste" | "paste0" => self.paste(name == "paste0", &arguments),
            "formatC" => {
                if !arguments.only_names(&["x", "width", "flag"]) {
                    return Err(Refusal::new("a `formatC` of an unsupported form"));
                }
                let value = arguments
                    .get("x", 0)
                    .ok_or(Refusal::new("a `formatC` without a value"))?;
                let value = self.value(value)?;
                if self.builder.ty(value) != Type::Int {
                    return Err(Refusal::new(
                        "a `formatC` of something other than an integer",
                    ));
                }
                let width = match arguments.named("width") {
                    Some(width) => {
                        constant_integer(width).ok_or(Refusal::new("a computed `formatC` width"))?
                    }
                    None => 0,
                };
                let zero = match arguments.named("flag") {
                    Some(flag) => match constant_string(flag).as_deref() {
                        Some("0") => true,
                        Some("") => false,
                        _ => return Err(Refusal::new("a `formatC` flag other than \"0\"")),
                    },
                    None => false,
                };
                Ok(self
                    .builder
                    .emit(Type::Str, Op::FormatPadded(value, width, zero)))
            }
            "sprintf" => {
                let [format, value] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a `sprintf` of an unsupported form"));
                };
                let format =
                    constant_string(format).ok_or(Refusal::new("a computed `sprintf` format"))?;
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
                            .ok_or(Refusal::new(
                                "a `sprintf` format the typed core does not know",
                            ))?;
                        let value = self.as_double(value)?;
                        Ok(self
                            .builder
                            .emit(Type::Str, Op::FormatFixed(value, decimals)))
                    }
                    _ => Err(Refusal::new(
                        "a `sprintf` format the typed core does not know",
                    )),
                }
            }
            "nchar" | "toupper" | "tolower" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a string function of an unsupported form"));
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
                    _ => Err(Refusal::new(
                        "a string function of something other than strings",
                    )),
                }
            }
            "startsWith" | "endsWith" => {
                let [a, affix] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed `startsWith`"));
                };
                let a = self.value(a)?;
                let affix = self.value(affix)?;
                if self.builder.ty(affix) != Type::Str {
                    return Err(Refusal::new("a `startsWith` of a computed prefix vector"));
                }
                let starts = name == "startsWith";
                match self.builder.ty(a) {
                    Type::Str => Ok(self.builder.emit(Type::Lgl, Op::Affix(starts, a, affix))),
                    Type::Vector {
                        item: Item::Str, ..
                    } => Ok(self
                        .builder
                        .emit(Type::vector(Item::Lgl), Op::VectorAffix(starts, a, affix))),
                    _ => Err(Refusal::new(
                        "a `startsWith` of something other than strings",
                    )),
                }
            }
            "[[" | "[" => {
                let [vector, index] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("an index of an unsupported form"));
                };
                let vector = self.value(vector)?;
                let ty = self.builder.ty(vector);
                if let Type::Record(shape) = ty {
                    // A tuple's item is a field, at a position the code spells as a constant.
                    let positional = interp::heap()
                        .layouts
                        .get(shapes::shape(shape).layout)
                        .is_some_and(|layout| {
                            layout.tag.is_none() && matches!(layout.fields, Fields::Positional(_))
                        });
                    let position = constant_integer(index).filter(|&position| position >= 1);
                    return match (name, positional, position) {
                        ("[[", true, Some(position)) => {
                            self.field(vector, shape, position as usize - 1)
                        }
                        _ => Err(Refusal::new(
                            "an index into a record, or into a tuple at a computed position",
                        )),
                    };
                }
                let index = self.value(index)?;
                if name == "["
                    && matches!(
                        self.builder.ty(index),
                        Type::Vector {
                            item: Item::Lgl,
                            ..
                        }
                    )
                {
                    return match ty {
                        Type::Vector { .. } => Ok(self.builder.emit(ty, Op::Select(vector, index))),
                        _ => Err(Refusal::new(
                            "a logical index into something other than a vector",
                        )),
                    };
                }
                self.known_items(vector)?;
                let index = self.integer_index(index)?;
                let item = ty
                    .items()
                    .ok_or(Refusal::new("an index into something other than a vector"))?;
                if name == "[" && matches!(ty, Type::List(_)) {
                    return Err(Refusal::new("a `[` of a list"));
                }
                Ok(self
                    .builder
                    .emit(Type::of_item(item), Op::Get(vector, index)))
            }
            // A typed list carries its item type, so the runtime's empty-list storage fix is moot.
            ".ry_items" => {
                let [list, hint] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed `.ry_items`"));
                };
                if list.kind() == SexpType::Language
                    && list.car().kind() == SexpType::Symbol
                    && rvalues::symbol_name(list.car()) == ".ry_array"
                    && list.cdr() == rvalues::nil()
                {
                    let hint = constant_string(hint);
                    let empty = Arguments { items: Vec::new() };
                    return self.list_literal(&empty, hint.as_deref(), None);
                }
                if self.is_library_call(list, "List.with_capacity", 1)? {
                    let [capacity] = Arguments::of(list).exactly(1)?[..] else {
                        return Err(Refusal::new("a malformed `List.with_capacity`"));
                    };
                    let capacity = self.value(capacity)?;
                    let capacity = self.integer_count(capacity)?;
                    let hint = constant_string(hint);
                    let empty = Arguments { items: Vec::new() };
                    return self.list_literal(&empty, hint.as_deref(), Some(capacity));
                }
                if self.is_library_call(list, "Dict.empty", 0)? {
                    let ty = constant_string(hint)
                        .as_deref()
                        .and_then(dict_type)
                        .ok_or(Refusal::new("an empty dictionary of an unknown type"))?;
                    return Ok(self.builder.emit(ty, Op::DictNew));
                }
                self.value(list)
            }
            ".ry_array" => self.list_literal(&arguments, None, None),
            "List.append" => self.append(&arguments),
            "List.concat" => self.concat(&arguments),
            "List.swap" => self.swap(&arguments),
            "List.is_empty" => {
                let [list] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `List.is_empty`"));
                };
                let list = self.value(list)?;
                if !matches!(self.builder.ty(list), Type::Vector { array: true, .. }) {
                    return Err(Refusal::new(
                        "a `List.is_empty` of something other than a ry list",
                    ));
                }
                let length = self.builder.emit(Type::I64, Op::Length(list));
                let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
                let empty = self
                    .builder
                    .emit(Type::Lgl, Op::Compare(Compare::Eq, Item::I64, length, zero));
                Ok(self.builder.emit(Type::Bool, Op::Truth(empty)))
            }
            "List.sublist" | "List.take_first" | "List.take_last" | "List.drop_first"
            | "List.drop_last" => self.slice(name, &arguments),
            "lapply" | "vapply" | "List.map" => self.map(name, &arguments),
            "List.sort_with" => self.sort_with(&arguments),
            ".ry_record" | ".ry_tuple" | ".ry_tag" => self.construct(name, &arguments),
            ".ry_update" => self.update(&arguments),
            "$" => {
                let [object, field] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed `$`"));
                };
                let object = self.value(object)?;
                let Type::Record(shape) = self.builder.ty(object) else {
                    return Err(Refusal::new("a `$` of something other than a record"));
                };
                let symbol = match field.kind() {
                    SexpType::Symbol => field,
                    SexpType::String => {
                        let text =
                            rvalues::string_at(field, 0).ok_or(Refusal::new("an NA field name"))?;
                        rvalues::install(&text).map_err(|_| Refusal::new("a field name"))?
                    }
                    _ => return Err(Refusal::new("a `$` of a computed field")),
                };
                let index = interp::heap()
                    .layouts
                    .get(shapes::shape(shape).layout)
                    .and_then(|layout| layout.names().iter().position(|&name| name == symbol))
                    .ok_or(Refusal::new("a field the record does not have"))?;
                self.field(object, shape, index)
            }
            "Result.with_default" => self.with_default(&arguments),
            "List.keep_oks" | "List.keep_errs" => {
                self.keep_results(name == "List.keep_oks", &arguments)
            }
            "Dict.get" => self.dict_get(&arguments),
            "Dict.insert" => self.dict_insert(&arguments),
            "Dict.to_list" => self.dict_to_list(&arguments),
            "Dict.len" | "Dict.keys" | "Dict.values" => {
                let [dict] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `Dict` function"));
                };
                let dict = self.value(dict)?;
                let Type::Dict { key, value } = self.builder.ty(dict) else {
                    return Err(Refusal::new(
                        "a `Dict` function of something other than a dictionary",
                    ));
                };
                Ok(match name {
                    "Dict.len" => self.builder.emit(Type::I64, Op::DictLen(dict)),
                    "Dict.keys" => self.builder.emit(
                        Type::Vector {
                            item: key,
                            array: true,
                        },
                        Op::DictKeys(dict),
                    ),
                    _ => self.builder.emit(
                        Type::Vector {
                            item: value,
                            array: true,
                        },
                        Op::DictValues(dict),
                    ),
                })
            }
            "Dict.contains" | "Dict.remove" => {
                let [dict, key] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed `Dict` function"));
                };
                let dict = self.value(dict)?;
                let key = self.value(key)?;
                let key = self.dict_key(dict, key)?;
                Ok(if name == "Dict.contains" {
                    self.builder.emit(Type::Bool, Op::DictContains(dict, key))
                } else {
                    let ty = self.builder.ty(dict);
                    self.builder.emit(ty, Op::DictRemove(dict, key))
                })
            }
            "Dict.update" => self.dict_update(&arguments),
            "Str.to_i64" | "Str.to_f64" => self.parse_number(name == "Str.to_i64", &arguments),
            ".ry_text" | "Num.to_str" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `Num.to_str`"));
                };
                let a = self.value(a)?;
                match (name, self.builder.ty(a)) {
                    (".ry_text", Type::Str) => Ok(a),
                    (".ry_text", Type::Dbl) | (_, Type::I64 | Type::Int) => {
                        Ok(self.builder.emit(Type::Str, Op::Format(a)))
                    }
                    _ => Err(Refusal::new(
                        "a `Num.to_str` of something other than a whole number",
                    )),
                }
            }
            "Str.concat" => {
                let [a, b] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed `Str.concat`"));
                };
                let a = self.value(a)?;
                let b = self.value(b)?;
                if self.builder.ty(a) != Type::Str || self.builder.ty(b) != Type::Str {
                    return Err(Refusal::new(
                        "a `Str.concat` of something other than strings",
                    ));
                }
                let empty = self.string("");
                Ok(self
                    .builder
                    .emit(Type::Str, Op::Concat(SmallVec::from_slice(&[a, b]), empty)))
            }
            "Str.repeat" => {
                let [a, count] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed `Str.repeat`"));
                };
                let a = self.value(a)?;
                let count = self.value(count)?;
                if self.builder.ty(a) != Type::Str {
                    return Err(Refusal::new(
                        "a `Str.repeat` of something other than a string",
                    ));
                }
                let count = self.to_i64(count)?;
                Ok(self.builder.emit(Type::Str, Op::RepeatStr(a, count)))
            }
            "Str.count_utf8_bytes"
            | "Str.to_utf8"
            | "Str.with_ascii_uppercased"
            | "Str.with_ascii_lowercased" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `Str` function"));
                };
                let a = self.value(a)?;
                if self.builder.ty(a) != Type::Str {
                    return Err(Refusal::new(
                        "a `Str` function of something other than a string",
                    ));
                }
                Ok(match name {
                    "Str.count_utf8_bytes" => self.builder.emit(Type::I64, Op::ByteCount(a)),
                    "Str.to_utf8" => self.builder.emit(
                        Type::Vector {
                            item: Item::I64,
                            array: true,
                        },
                        Op::ToUtf8(a),
                    ),
                    "Str.with_ascii_uppercased" => {
                        self.builder.emit(Type::Str, Op::Text(Text::AsciiUpper, a))
                    }
                    _ => self.builder.emit(Type::Str, Op::Text(Text::AsciiLower, a)),
                })
            }
            "Str.from_utf8_lossy" => {
                let [bytes] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `Str` function"));
                };
                let bytes = self.value(bytes)?;
                let numbers = Type::Vector {
                    item: Item::I64,
                    array: true,
                };
                if self.builder.ty(bytes) != numbers {
                    return Err(Refusal::new(
                        "a `Str.from_utf8_lossy` of something other than a list of I64s",
                    ));
                }
                Ok(self.builder.emit(Type::Str, Op::FromUtf8Lossy(bytes)))
            }
            "Str.starts_with" | "Str.ends_with" | "Str.split_on" | "Str.join_with" => {
                let [a, b] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed `Str` function"));
                };
                let a = self.value(a)?;
                let b = self.value(b)?;
                if self.builder.ty(b) != Type::Str {
                    return Err(Refusal::new(
                        "a `Str` function of something other than strings",
                    ));
                }
                let strings = Type::Vector {
                    item: Item::Str,
                    array: true,
                };
                match (name, self.builder.ty(a)) {
                    ("Str.starts_with" | "Str.ends_with", Type::Str) => Ok(self
                        .builder
                        .emit(Type::Lgl, Op::Affix(name == "Str.starts_with", a, b))),
                    ("Str.split_on", Type::Str) => Ok(self.builder.emit(strings, Op::Split(a, b))),
                    ("Str.join_with", ty) if ty == strings => {
                        Ok(self.builder.emit(Type::Str, Op::Join(a, b)))
                    }
                    _ => Err(Refusal::new(
                        "a `Str` function of something other than strings",
                    )),
                }
            }
            "unlist" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("an `unlist` of an unsupported form"));
                };
                let a = self.value(a)?;
                match self.builder.ty(a) {
                    Type::List(item) => Ok(self
                        .builder
                        .emit(Type::vector(item), Op::Relabel(a, Type::vector(item)))),
                    Type::Vector { .. } => Ok(a),
                    _ => Err(Refusal::new(
                        "an `unlist` of something other than a list of scalars",
                    )),
                }
            }
            "Num.from_r" | "Str.from_r" | "Bool.from_r" | "List.from_r" => {
                let [inner] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a conversion of an unsupported form"));
                };
                self.r_use = Some(RUse::Converted(Conversion { call, function }));
                let value = self.value(inner);
                self.r_use = None;
                let value = value?;
                if self.converted.take() == Some(value) {
                    return Ok(value);
                }
                // A conversion of a value that already has the type it gives is that value.
                let ty = self.builder.ty(value);
                let identity = match name {
                    "Num.from_r" => matches!(ty, Type::Dbl | Type::I64),
                    "Str.from_r" => ty == Type::Str,
                    "Bool.from_r" => ty == Type::Bool,
                    _ => matches!(ty, Type::Vector { array: true, .. }),
                };
                if identity {
                    return Ok(value);
                }
                Err(Refusal(Cow::Owned(format!(
                    "it calls `{name}` of a value typed code computed, which the typed core does \
                     not compile yet"
                ))))
            }
            // A standard library function's module is capitalized, as in `Num.to_r`.
            _ if name.starts_with(|first: char| first.is_ascii_uppercase()) => {
                Err(Refusal(Cow::Owned(format!(
                    "it calls `{name}`, which the typed core does not compile yet"
                ))))
            }
            _ => match r_use {
                Some(r_use) => self.r_call(call, function, r_use),
                None => Err(r_value_refusal(call.car())),
            },
        }
    }

    /// `lapply(x, f)`, `List.map(x, f)` and `vapply(x, f, value)` with `f` a function literal or
    /// a local function, inlined into a loop over the items.
    unsafe fn map(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let (vector, function, template) =
            match (name, &arguments.exactly(arguments.items.len())?[..]) {
                ("vapply", &[vector, function, template]) => (vector, function, Some(template)),
                ("lapply" | "List.map", &[vector, function]) => (vector, function, None),
                _ => return Err(Refusal::new("a map of an unsupported form")),
            };
        let callable = self.function_argument(function)?;
        let vector = self.value(vector)?;
        let ty = self.builder.ty(vector);
        let item = ty
            .items()
            .ok_or(Refusal::new("a map over something that is not a vector"))?;
        if name == "vapply" && item == Item::Str {
            // `vapply` names its result by a character input, which the typed vector cannot hold.
            return Err(Refusal::new("a `vapply` over strings"));
        }
        let declared = match template {
            Some(template) => Some(template_item(template).ok_or(Refusal::new(
                "a `vapply` template the typed core does not know",
            ))?),
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
            .apply(callable, &[element])?
            .ok_or(Refusal::new("a mapped function that never completes"))?;
        let value_item = self.builder.ty(value).item().ok_or(Refusal::new(
            "a mapped function whose value is not a scalar",
        ))?;
        let result_item = declared.unwrap_or(value_item);
        let value = self.convert_scalar(value, result_item)?;
        let output_ty = if name == "vapply" {
            Type::vector(result_item)
        } else if to_array {
            if matches!(result_item, Item::Int | Item::Lgl) {
                return Err(Refusal::new("a `List.map` to a list of R scalars"));
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

    /// `List.sort_with(list, compare)` as a stable bottom-up merge sort, with `compare` inlined
    /// once into the merge step: each pass merges runs of `width` items from one buffer into the
    /// other, and the buffers trade places after it. A right item goes first only when `compare`
    /// of the left and the right item is `:GT`, which keeps equal items in order.
    unsafe fn sort_with(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, function] = arguments.exactly(2)?[..] else {
            return Err(Refusal::new("a malformed `List.sort_with`"));
        };
        let list = self.value(list)?;
        let ty = self.builder.ty(list);
        let Type::Vector { item, array: true } = ty else {
            return Err(Refusal::new(
                "a `List.sort_with` of something that is not a list",
            ));
        };
        let callable = self.function_argument(function)?;
        let greater =
            shapes::intern(tag_layout("GT", 0)?, &[]).ok_or(Refusal::new("a comparison result"))?;
        let greater = self.int(greater as i32);
        let one = self.int(1);
        let source = self.builder.local(ty);
        self.builder.push(Some(source), Op::Copy(list));
        let count = self.builder.emit(Type::Int, Op::Length(source));
        let end = self
            .builder
            .emit(Type::Int, Op::Arith(Arith::Add, Item::Int, count, one));
        let fresh = self
            .builder
            .emit(Type::vector(item), Op::NewVector(item, count));
        let target = self.builder.local(ty);
        self.builder.push(Some(target), Op::Relabel(fresh, ty));
        let width = self.builder.local(Type::Int);
        self.builder.push(Some(width), Op::Copy(one));
        let (start, left, right, slot) = (
            self.builder.local(Type::Int),
            self.builder.local(Type::Int),
            self.builder.local(Type::Int),
            self.builder.local(Type::Int),
        );
        let (middle, stop) = (self.builder.local(Type::Int), self.builder.local(Type::Int));
        let done = self.builder.block();

        let passes = self.loop_header();
        let more = self.int_test(Compare::Lt, width, count);
        let pass = self.builder.block();
        self.builder.terminate(Terminator::Branch {
            condition: more,
            then: pass,
            otherwise: done,
        });
        self.builder.switch_to(pass);
        self.builder.push(Some(start), Op::Copy(one));
        let pass_end = self.builder.block();
        let runs = self.loop_header();
        let past = self.int_test(Compare::Gt, start, count);
        let run = self.builder.block();
        self.builder.terminate(Terminator::Branch {
            condition: past,
            then: pass_end,
            otherwise: run,
        });
        self.builder.switch_to(run);
        let after_left = self
            .builder
            .emit(Type::Int, Op::Arith(Arith::Add, Item::Int, start, width));
        self.int_min(middle, after_left, end);
        let after_right = self.builder.emit(
            Type::Int,
            Op::Arith(Arith::Add, Item::Int, after_left, width),
        );
        self.int_min(stop, after_right, end);
        self.builder.push(Some(left), Op::Copy(start));
        self.builder.push(Some(right), Op::Copy(middle));
        self.builder.push(Some(slot), Op::Copy(start));
        let run_end = self.builder.block();
        let merges = self.loop_header();
        let full = self.int_test(Compare::Ge, slot, stop);
        let merge = self.builder.block();
        self.builder.terminate(Terminator::Branch {
            condition: full,
            then: run_end,
            otherwise: merge,
        });
        self.builder.switch_to(merge);
        let (take_left, take_right, compare) = (
            self.builder.block(),
            self.builder.block(),
            self.builder.block(),
        );
        let left_spent = self.int_test(Compare::Ge, left, middle);
        let check_right = self.builder.block();
        self.builder.terminate(Terminator::Branch {
            condition: left_spent,
            then: take_right,
            otherwise: check_right,
        });
        self.builder.switch_to(check_right);
        let right_spent = self.int_test(Compare::Ge, right, stop);
        self.builder.terminate(Terminator::Branch {
            condition: right_spent,
            then: take_left,
            otherwise: compare,
        });
        self.builder.switch_to(compare);
        let a = self
            .builder
            .emit(Type::of_item(item), Op::Get(source, left));
        let b = self
            .builder
            .emit(Type::of_item(item), Op::Get(source, right));
        let order = self
            .apply(callable, &[a, b])?
            .ok_or(Refusal::new("a comparison that never completes"))?;
        if !matches!(self.builder.ty(order), Type::Tag(_)) {
            return Err(Refusal::new("a comparison whose answer is not a tag"));
        }
        let shape = self.builder.emit(Type::Int, Op::ShapeOf(order));
        let after = self.builder.emit(Type::Bool, Op::Same(shape, greater));
        self.builder.terminate(Terminator::Branch {
            condition: after,
            then: take_right,
            otherwise: take_left,
        });
        let next = self.builder.block();
        for (block, from) in [(take_left, left), (take_right, right)] {
            self.builder.switch_to(block);
            let value = self
                .builder
                .emit(Type::of_item(item), Op::Get(source, from));
            let updated = self.builder.emit(ty, Op::Set(target, slot, value));
            self.builder.push(Some(target), Op::Copy(updated));
            let moved = self
                .builder
                .emit(Type::Int, Op::Arith(Arith::Add, Item::Int, from, one));
            self.builder.push(Some(from), Op::Copy(moved));
            self.builder.terminate(Terminator::Jump(next));
        }
        self.builder.switch_to(next);
        let filled = self
            .builder
            .emit(Type::Int, Op::Arith(Arith::Add, Item::Int, slot, one));
        self.builder.push(Some(slot), Op::Copy(filled));
        self.builder.terminate(Terminator::Jump(merges));
        self.builder.switch_to(run_end);
        self.builder.push(Some(start), Op::Copy(stop));
        self.builder.terminate(Terminator::Jump(runs));
        self.builder.switch_to(pass_end);
        let merged = self.builder.emit(ty, Op::Copy(target));
        self.builder.push(Some(target), Op::Copy(source));
        self.builder.push(Some(source), Op::Copy(merged));
        let doubled = self
            .builder
            .emit(Type::Int, Op::Arith(Arith::Add, Item::Int, width, width));
        self.builder.push(Some(width), Op::Copy(doubled));
        self.builder.terminate(Terminator::Jump(passes));
        self.builder.switch_to(done);
        Ok(source)
    }

    /// Whether `a op b` holds for two integers that are not `NA`, as a branch condition.
    fn int_test(&mut self, op: Compare, a: Local, b: Local) -> Local {
        let test = self
            .builder
            .emit(Type::Lgl, Op::Compare(op, Item::Int, a, b));
        self.builder.emit(Type::Bool, Op::Truth(test))
    }

    /// Assigns `destination` the smaller of two integers that are not `NA`.
    fn int_min(&mut self, destination: Local, a: Local, b: Local) {
        let smaller = self.int_test(Compare::Lt, a, b);
        let (first, second, join) = (
            self.builder.block(),
            self.builder.block(),
            self.builder.block(),
        );
        self.builder.terminate(Terminator::Branch {
            condition: smaller,
            then: first,
            otherwise: second,
        });
        for (block, value) in [(first, a), (second, b)] {
            self.builder.switch_to(block);
            self.builder.push(Some(destination), Op::Copy(value));
            self.builder.terminate(Terminator::Jump(join));
        }
        self.builder.switch_to(join);
    }

    /// `List.count_if(list, predicate)` as an `I64` count, or `List.walk(list, state, f)` as the
    /// state each item's step leaves, in a loop over the items with the function inlined.
    unsafe fn walk(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let counting = name == "List.count_if";
        let (list, state, function) =
            match (counting, &arguments.exactly(arguments.items.len())?[..]) {
                (true, &[list, predicate]) => (list, None, predicate),
                (false, &[list, state, f]) => (list, Some(state), f),
                _ => return Err(Refusal::new("a walk of an unsupported form")),
            };
        let list = self.value(list)?;
        let ty = self.builder.ty(list);
        let item = match ty {
            Type::Vector { item, array: true } | Type::List(item) => item,
            _ => return Err(Refusal::new("a walk over something that is not a list")),
        };
        let callable = self.function_argument(function)?;
        // Counting a list of logicals with the identity counts its true items, which the fusion
        // pass can then compute from the comparisons that built the list without building it.
        if counting
            && ty
                == (Type::Vector {
                    item: Item::Lgl,
                    array: true,
                })
            && let Callable::Inline(formals, body, _) = callable
            && let [(name, _)] = rvalues::pairlist_items(formals)[..]
            && body == name
        {
            return Ok(self.builder.emit(
                Type::I64,
                Op::CountTrue {
                    program: SmallVec::new(),
                    inputs: SmallVec::from_slice(&[list]),
                },
            ));
        }
        let state = match state {
            Some(state) if self.is_library_call(state, "Dict.empty", 0)? => {
                // An empty dictionary's type is the one the step function declares for its state.
                let Callable::Inline(formals, body, _) = callable else {
                    return Err(Refusal::new("a walk from `Dict.empty()` with a named step"));
                };
                let ty = interp::annotations::formal_annotation(formals, body, 0)
                    .as_deref()
                    .and_then(dict_type)
                    .ok_or(Refusal::new(
                        "a walk from `Dict.empty()` whose state type is unknown",
                    ))?;
                self.builder.emit(ty, Op::DictNew)
            }
            Some(state) => self.value(state)?,
            None => self.builder.emit(Type::I64, Op::Const(Constant::I64(0))),
        };
        let state_ty = self.builder.ty(state);
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
                .apply(callable, &[element])?
                .ok_or(Refusal::new("a predicate that never completes"))?;
            // The interpreter raises the error for an answer other than `TRUE` or `FALSE`.
            let answer = match self.builder.ty(answer) {
                Type::Bool => answer,
                Type::Lgl => self.builder.emit(Type::Bool, Op::Truth(answer)),
                _ => return Err(Refusal::new("a predicate whose answer is not a logical")),
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
                .apply(callable, &[total, element])?
                .ok_or(Refusal::new("a walk step that never completes"))?;
            if self.builder.ty(step) != state_ty {
                return Err(Refusal::new("a walk whose state changes type"));
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

    /// A function argument: a literal or a local function, which inlines, or a typed closure,
    /// which is called.
    unsafe fn function_argument(&mut self, function: Sexp) -> Result<Callable, Refusal> {
        if function.kind() == SexpType::Symbol {
            match self.lookup(function) {
                Some(Binding::Function(formals, body, scope)) => {
                    return Ok(Callable::Inline(formals, body, scope));
                }
                Some(Binding::Value(_)) => {
                    return Err(Refusal::new("a mapped function held in a variable"));
                }
                None => {}
            }
            let speculation = resolve_function(function, self.env)
                .ok_or(Refusal::new("a mapped function that does not resolve"))?;
            let found = speculation.function;
            if found.kind() != SexpType::Closure || !interp::annotations::is_typed(found) {
                return Err(Refusal::new(
                    "a mapped function that is not a typed closure",
                ));
            }
            self.speculations.push(speculation);
            return Ok(Callable::Typed(found));
        }
        if function.kind() == SexpType::Language
            && function.car().kind() == SexpType::Symbol
            && rvalues::symbol_name(function.car()) == "function"
        {
            let parts = rvalues::pairlist_items(function.cdr());
            if let (Some(&(_, formals)), Some(&(_, body))) = (parts.first(), parts.get(1)) {
                return Ok(Callable::Inline(formals, body, self.scopes.len() - 1));
            }
        }
        Err(Refusal::new(
            "a mapped function that is not a function literal",
        ))
    }

    /// Calls a function argument with these arguments.
    unsafe fn apply(
        &mut self,
        callable: Callable,
        arguments: &[Local],
    ) -> Result<Option<Local>, Refusal> {
        match callable {
            Callable::Inline(formals, body, scope) => self.inline(formals, body, scope, arguments),
            Callable::Typed(function) => self.typed_call(function, arguments).map(Some),
        }
    }

    /// Whether `expression` is a call of the standard library's `name` with `arity` arguments,
    /// which the lookup assumes keeps finding it.
    unsafe fn is_library_call(
        &mut self,
        expression: Sexp,
        name: &str,
        arity: usize,
    ) -> Result<bool, Refusal> {
        if expression.kind() != SexpType::Language
            || expression.car().kind() != SexpType::Symbol
            || rvalues::symbol_name(expression.car()) != name
            || Arguments::of(expression).items.len() != arity
            || self.lookup(expression.car()).is_some()
        {
            return Ok(false);
        }
        match resolve_function(expression.car(), self.env) {
            Some(speculation) if is_base_function(&speculation) => {
                self.speculations.push(speculation);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    // Records, tuples and tags.

    /// `.ry_record(...)`, `.ry_tuple(...)` or `.ry_tag(name, ...)`: a new box of its fields.
    unsafe fn construct(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let items = &arguments.items[..];
        let (tag, items) = if name == ".ry_tag" {
            let ((None, tag), payload) = items
                .split_first()
                .ok_or(Refusal::new("a tag without a name"))?
            else {
                return Err(Refusal::new("a tag whose name is an argument"));
            };
            (Some(*tag), payload)
        } else {
            (None, items)
        };
        let names: SmallVec<[Sexp; 4]> = items
            .iter()
            .map(|(tag, _)| match tag {
                Some(tag) => rvalues::install(tag).map_err(|_| Refusal::new("a field name")),
                None => Ok(rvalues::nil()),
            })
            .collect::<Result<_, _>>()?;
        let (layout, slots) =
            interp::records::constructor_layout(tag, &names, name == ".ry_record")
                .ok_or(Refusal::new("a constructor the interpreter rejects"))?;
        let mut fields: SmallVec<[(usize, Local); 4]> = SmallVec::new();
        for (&(_, item), &slot) in items.iter().zip(&slots) {
            if item == rvalues::symbols().dots || item == rvalues::missing_arg() {
                return Err(Refusal::new("a constructor with `...`"));
            }
            fields.push((slot, self.value(item)?));
        }
        fields.sort_by_key(|&(slot, _)| slot);
        let fields: SmallVec<[Local; 4]> = fields.iter().map(|&(_, field)| field).collect();
        self.boxed(layout, tag.is_some(), &fields)
    }

    /// `(..record, name = value, ...)`: a new record with the base's fields, the named ones
    /// replaced. A later value for the same name wins, as in the interpreter.
    unsafe fn update(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let Some(((None, base), fields)) = arguments.items.split_first() else {
            return Err(Refusal::new("a malformed record update"));
        };
        let base = self.value(*base)?;
        let Type::Record(shape) = self.builder.ty(base) else {
            return Err(Refusal::new("an update of something other than a record"));
        };
        let layout = shapes::shape(shape).layout;
        let names: SmallVec<[Sexp; 4]> = interp::heap()
            .layouts
            .get(layout)
            .map(|layout| layout.names().iter().copied().collect())
            .ok_or(Refusal::new("a record without a layout"))?;
        let mut replaced: SmallVec<[Option<Local>; 4]> = SmallVec::from_elem(None, names.len());
        for (name, value) in fields {
            let name = name
                .as_deref()
                .ok_or(Refusal::new("a record update field without a name"))?;
            let symbol = rvalues::install(name).map_err(|_| Refusal::new("a field name"))?;
            let index = names
                .iter()
                .position(|&field| field == symbol)
                .ok_or(Refusal::new(
                    "an update of a field the record does not have",
                ))?;
            let value = self.value(*value)?;
            if let Some(slot) = replaced.get_mut(index) {
                *slot = Some(value);
            }
        }
        let mut values: SmallVec<[Local; 4]> = SmallVec::new();
        for (index, replacement) in replaced.into_iter().enumerate() {
            values.push(match replacement {
                Some(value) => value,
                None => self.field(base, shape, index)?,
            });
        }
        self.boxed(layout, false, &values)
    }

    /// A new box of `layout` holding `fields` in slot order, a tag when `tagged`.
    unsafe fn boxed(
        &mut self,
        layout: u32,
        tagged: bool,
        fields: &[Local],
    ) -> Result<Local, Refusal> {
        let mut items: SmallVec<[Item; 4]> = SmallVec::new();
        for &field in fields {
            self.usable(field)?;
            let item = self
                .builder
                .ty(field)
                .held_item()
                .ok_or(Refusal::new("a field a box cannot hold"))?;
            items.push(item);
        }
        let shape = shapes::intern(layout, &items)
            .ok_or(Refusal::new("a value with more fields than a box holds"))?;
        let ty = if tagged {
            // Values of this shape exist once the code runs, so a match over any tag tests for it.
            if !self.constructed.contains(&shape) {
                self.constructed.push(shape);
            }
            Type::Tag(shapes::union_of(&[shape]))
        } else {
            Type::Record(shape)
        };
        if fields.is_empty() {
            let constant = Constant::Object(shapes::constant(shape));
            return Ok(self.builder.emit(ty, Op::Const(constant)));
        }
        Ok(self
            .builder
            .emit(ty, Op::Construct(shape, SmallVec::from_slice(fields))))
    }

    /// The tag `:name(payload...)` with a positional payload.
    unsafe fn tag(&mut self, name: &str, payload: &[Local]) -> Result<Local, Refusal> {
        let layout = tag_layout(name, payload.len() as u32)?;
        self.boxed(layout, true, payload)
    }

    /// Field `index` of a box known to have `shape`.
    fn field(&mut self, value: Local, shape: u32, index: usize) -> Result<Local, Refusal> {
        let item = *shapes::shape(shape)
            .fields
            .get(index)
            .ok_or(Refusal::new("a field the shape does not have"))?;
        Ok(self
            .builder
            .emit(Type::of_item(item), Op::Field(value, shape, index as u32)))
    }

    /// The shapes a record or tag of `ty` may have, and whether that is all of them. Of any tag,
    /// those are the confirmed shapes with one of `layouts` or one of `tags`, which the code
    /// relies on from now on.
    fn shapes_of(
        &mut self,
        ty: Type,
        layouts: &[u32],
        tags: &[u32],
    ) -> Result<(SmallVec<[u32; 4]>, bool), Refusal> {
        match ty {
            Type::Record(shape) => Ok((SmallVec::from_slice(&[shape]), true)),
            Type::Tag(union) => match shapes::members(union) {
                Some(members) => Ok((members, true)),
                None => {
                    self.epoch.get_or_insert(shapes::epoch());
                    let heap = interp::heap();
                    let mut candidates = shapes::confirmed();
                    for &shape in &self.constructed {
                        if !candidates.contains(&shape) {
                            candidates.push(shape);
                        }
                    }
                    let found = candidates
                        .into_iter()
                        .filter(|&shape| {
                            let layout = shapes::shape(shape).layout;
                            layouts.contains(&layout)
                                || heap
                                    .layouts
                                    .get(layout)
                                    .and_then(|layout| layout.tag)
                                    .is_some_and(|tag| tags.contains(&tag))
                        })
                        .collect();
                    Ok((found, false))
                }
            },
            _ => Err(Refusal::new("a value that is not a record or tag")),
        }
    }

    /// Branches on the shape of the box `value`, lowering `arm` once for each of `shapes` and,
    /// unless they are all the box may have, once with `None` for any other, and joins the
    /// values the arms complete with.
    ///
    /// `target` is the type the switch's value must have, the function's declared result when
    /// that value is the function's result. An arm for a shape only confirmed somewhere whose
    /// value cannot have it gives up for that shape alone, as an arm that does not lower does.
    /// A tag from R is any tag, whatever the annotation says, so ill-typed R code can confirm
    /// `:One(F64)` beside the `:One(I64)` a function declares, and without a target the arms
    /// would disagree and the whole function would refuse for the rest of the session.
    unsafe fn switch_shapes(
        &mut self,
        value: Local,
        shapes: &[u32],
        exhaustive: bool,
        target: Option<Type>,
        arm: &mut dyn FnMut(&mut Self, Option<u32>) -> Result<Option<Local>, Refusal>,
    ) -> Result<Option<Local>, Refusal> {
        let mut cases: SmallVec<[Option<u32>; 4]> =
            shapes.iter().map(|&shape| Some(shape)).collect();
        if !exhaustive {
            cases.push(None);
        }
        let shape_of = self.builder.emit(Type::Int, Op::ShapeOf(value));
        let mut ends: SmallVec<[(BlockId, Local); 4]> = SmallVec::new();
        let mut test = self.builder.current;
        for (position, &case) in cases.iter().enumerate() {
            self.builder.switch_to(test);
            let body = self.builder.block();
            match case {
                Some(shape) if position + 1 < cases.len() => {
                    let expected = self.int(shape as i32);
                    let same = self.builder.emit(Type::Bool, Op::Same(shape_of, expected));
                    let next = self.builder.block();
                    self.builder.terminate(Terminator::Branch {
                        condition: same,
                        then: body,
                        otherwise: next,
                    });
                    test = next;
                }
                _ => self.builder.terminate(Terminator::Jump(body)),
            }
            self.builder.switch_to(body);
            let saved = self.enter_control();
            // A shape only confirmed somewhere, which the code may never see, whose arm does not
            // lower (a field of another type than the annotation says, say) gives up for that
            // shape alone rather than for the whole function.
            let checkpoint = (!exhaustive && case.is_some()).then(|| self.checkpoint());
            let completed = match (arm(self, case), checkpoint) {
                (Err(refusal), Some(checkpoint)) => {
                    if !self.rewind(checkpoint, body) {
                        return Err(refusal);
                    }
                    Ok(None)
                }
                (Ok(Some(completed)), checkpoint) if target.is_some() => {
                    match (self.toward(completed, target), checkpoint) {
                        (Some(completed), _) => Ok(Some(completed)),
                        (None, Some(checkpoint)) => {
                            if self.rewind(checkpoint, body) {
                                Ok(None)
                            } else {
                                Ok(Some(completed))
                            }
                        }
                        (None, None) => Ok(Some(completed)),
                    }
                }
                (completed, _) => completed,
            };
            self.leave_control(saved);
            if let Some(completed) = completed? {
                self.usable(completed)?;
                ends.push((self.builder.current, completed));
            }
        }
        if cases.is_empty() {
            self.builder.terminate(Terminator::Fail);
        }
        let join = self.builder.block();
        let mut ty: Option<Option<Type>> = None;
        for &(_, completed) in &ends {
            let found = self.builder.ty(completed);
            ty = Some(match ty {
                None => Some(found),
                Some(held) => held.and_then(|held| joined(held, found)),
            });
        }
        let result = match ty {
            None => None,
            Some(ty) => {
                let result = self.builder.local(ty.unwrap_or(Type::Null));
                for &(end, completed) in &ends {
                    self.builder.switch_to(end);
                    let op = match ty {
                        Some(_) => Op::Copy(completed),
                        None => Op::Const(Constant::Null),
                    };
                    self.builder.push(Some(result), op);
                    self.builder.terminate(Terminator::Jump(join));
                }
                // The arms disagree, which is fine where nothing reads the value.
                if ty.is_none() {
                    self.unusable.push(result);
                }
                Some(result)
            }
        };
        self.builder.switch_to(join);
        if result.is_none() {
            self.builder.terminate(Terminator::Fail);
        }
        Ok(result)
    }

    /// `value` as a value of `target`, converting a number to the `I64` a function declares as
    /// its exit does, or `None` when it cannot be one.
    unsafe fn toward(&mut self, value: Local, target: Option<Type>) -> Option<Local> {
        let target = target?;
        let value = match (target, self.builder.ty(value)) {
            (Type::I64, Type::Int | Type::Dbl) => self.to_i64(value).ok()?,
            _ => value,
        };
        joined(target, self.builder.ty(value)).map(|_| value)
    }

    fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            scopes: self.scopes.clone(),
            loops: self.loops.len(),
            inlined: self.inlined.clone(),
            speculations: self.speculations.len(),
            nesting: self.nesting,
            control: self.control,
            unusable: self.unusable.len(),
            returned: self.returned,
            constructed: self.constructed.len(),
            r_calls: self.r_calls.len(),
            blocks: self.builder.function.blocks.len(),
        }
    }

    /// Forgets everything lowered since `checkpoint`, which an arm starting at block `body` took,
    /// and makes `body` give up. `false` when the arm changed a block that existed before it, so
    /// it cannot be forgotten.
    fn rewind(&mut self, checkpoint: Checkpoint, body: BlockId) -> bool {
        let blocks = &mut self.builder.function.blocks;
        let kept = checkpoint.blocks;
        if blocks.iter().take(kept).enumerate().any(|(index, block)| {
            index != body.0 as usize
                && block
                    .terminator
                    .successors()
                    .iter()
                    .any(|target| target.0 as usize >= kept)
        }) {
            return false;
        }
        blocks.truncate(kept);
        let Some(entry) = blocks.get_mut(body.0 as usize) else {
            return false;
        };
        entry.insts.clear();
        entry.terminator = Terminator::Fail;
        self.builder.current = body;
        self.scopes = checkpoint.scopes;
        self.loops.truncate(checkpoint.loops);
        self.inlined = checkpoint.inlined;
        self.speculations.truncate(checkpoint.speculations);
        self.nesting = checkpoint.nesting;
        self.control = checkpoint.control;
        self.unusable.truncate(checkpoint.unusable);
        self.returned = checkpoint.returned;
        self.constructed.truncate(checkpoint.constructed);
        self.r_calls.truncate(checkpoint.r_calls);
        true
    }

    /// `match (value) { pattern => body ... }` over a record or tag, decided for each shape the
    /// value may have as the interpreter would decide it for a value of that shape.
    unsafe fn match_expression(&mut self, call: Sexp) -> Result<Option<Local>, Refusal> {
        let items = rvalues::pairlist_items(call.cdr());
        let Some((&(scrutinee_tag, scrutinee), arms)) = items.split_first() else {
            return Err(Refusal::new("a malformed `match`"));
        };
        if scrutinee_tag != rvalues::nil() {
            return Err(Refusal::new("a `match` with named arguments"));
        }
        let arrow = rvalues::install("=>").map_err(|_| Refusal::new("a malformed `match`"))?;
        let mut parsed: SmallVec<[(Sexp, Sexp); 4]> = SmallVec::new();
        for &(tag, arm) in arms {
            if tag != rvalues::nil() || arm.kind() != SexpType::Language || arm.car() != arrow {
                return Err(Refusal::new("a malformed `match` arm"));
            }
            match rvalues::pairlist_items(arm.cdr())[..] {
                [(pattern_tag, pattern), (body_tag, body)]
                    if pattern_tag == rvalues::nil() && body_tag == rvalues::nil() =>
                {
                    parsed.push((pattern, body));
                }
                [_, _, _] => return Err(Refusal::new("a `match` arm with a guard")),
                _ => return Err(Refusal::new("a malformed `match` arm")),
            }
        }
        let value = self.value(scrutinee)?;
        let (mut layouts, mut tags): (SmallVec<[u32; 4]>, SmallVec<[u32; 4]>) = Default::default();
        for &(pattern, _) in &parsed {
            pattern_layouts(pattern, &mut layouts, &mut tags)?;
        }
        let (candidates, exhaustive) = self.shapes_of(self.builder.ty(value), &layouts, &tags)?;
        let target = (self.inlined.is_empty() && self.tails.contains(&call))
            .then_some(self.declared)
            .flatten();
        self.switch_shapes(
            value,
            &candidates,
            exhaustive,
            target,
            &mut |lowering, shape| {
                for &(pattern, body) in &parsed {
                    let mut bindings: SmallVec<[(Sexp, Option<usize>); 4]> = SmallVec::new();
                    if !pattern_matches(pattern, shape, &mut bindings)? {
                        continue;
                    }
                    for (symbol, field) in bindings {
                        let bound = match (field, shape) {
                            (Some(index), Some(shape)) => lowering.field(value, shape, index)?,
                            _ => value,
                        };
                        lowering.assign(symbol, bound)?;
                    }
                    return lowering.expression(body);
                }
                // The interpreter raises the error that no arm matches.
                lowering.builder.terminate(Terminator::Fail);
                let unreachable = lowering.builder.block();
                lowering.builder.switch_to(unreachable);
                Ok(None)
            },
        )
    }

    /// `Result.with_default(result, default)`.
    unsafe fn with_default(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [result, default] = arguments.exactly(2)?[..] else {
            return Err(Refusal::new("a malformed `Result.with_default`"));
        };
        let result = self.value(result)?;
        let default = self.value(default)?;
        let (ok, err) = (tag_layout("Ok", 1)?, tag_layout("Err", 1)?);
        let (candidates, exhaustive) = self.shapes_of(self.builder.ty(result), &[ok, err], &[])?;
        let value = self.switch_shapes(
            result,
            &candidates,
            exhaustive,
            None,
            &mut |lowering, shape| {
                let layout = shape.map(|shape| shapes::shape(shape).layout);
                match (shape, layout) {
                    (Some(shape), Some(layout)) if layout == ok => {
                        lowering.field(result, shape, 0).map(Some)
                    }
                    (_, Some(layout)) if layout == err => Ok(Some(default)),
                    _ => {
                        lowering.builder.terminate(Terminator::Fail);
                        let unreachable = lowering.builder.block();
                        lowering.builder.switch_to(unreachable);
                        Ok(None)
                    }
                }
            },
        )?;
        let value = value.ok_or(Refusal::new("a `Result.with_default` of no result"))?;
        self.usable(value)?;
        Ok(value)
    }

    /// `List.keep_oks(list, f)` or `List.keep_errs(list, f)`: the payloads of the results `f`
    /// gives that are `:Ok`, or `:Err`, as a list.
    unsafe fn keep_results(&mut self, oks: bool, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, function] = arguments.exactly(2)?[..] else {
            return Err(Refusal::new("a malformed `List.keep_oks`"));
        };
        let list = self.value(list)?;
        let callable = self.function_argument(function)?;
        let ty = self.builder.ty(list);
        let item = match ty {
            Type::Vector { item, array: true } | Type::List(item) => item,
            _ => {
                return Err(Refusal::new(
                    "a `List.keep_oks` over something that is not a list",
                ));
            }
        };
        let (ok, err) = (tag_layout("Ok", 1)?, tag_layout("Err", 1)?);
        let kept = if oks { ok } else { err };
        let held = self.builder.emit(ty, Op::Copy(list));
        let count = self.builder.emit(Type::Int, Op::Length(held));
        let preheader = self.builder.current;
        // The output's item type is the payload's, known once the function is lowered.
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
        let result = self
            .apply(callable, &[element])?
            .ok_or(Refusal::new("a function that never completes"))?;
        let (candidates, exhaustive) = self.shapes_of(self.builder.ty(result), &[ok, err], &[])?;
        let mut payload = None;
        for &shape in &candidates {
            let shape = shapes::shape(shape);
            if shape.layout == kept {
                let found = shape.fields.first().copied();
                if payload.is_some_and(|payload| Some(payload) != found) {
                    return Err(Refusal::new("results whose payloads have different types"));
                }
                payload = found;
            }
        }
        let payload = payload.ok_or(Refusal::new("results that are never kept"))?;
        let output_ty = Type::Vector {
            item: payload,
            array: true,
        };
        self.builder.function.locals[output.0 as usize] = output_ty;
        self.switch_shapes(
            result,
            &candidates,
            exhaustive,
            None,
            &mut |lowering, shape| {
                let layout = shape.map(|shape| shapes::shape(shape).layout);
                match (shape, layout) {
                    (Some(shape), Some(layout)) if layout == kept => {
                        let field = lowering.field(result, shape, 0)?;
                        let pushed = lowering.builder.emit(output_ty, Op::Push(output, field));
                        lowering.builder.push(Some(output), Op::Copy(pushed));
                    }
                    (_, Some(layout)) if layout == ok || layout == err => {}
                    _ => {
                        // Something other than a result, which the interpreter rejects.
                        lowering.builder.terminate(Terminator::Fail);
                        let unreachable = lowering.builder.block();
                        lowering.builder.switch_to(unreachable);
                        return Ok(None);
                    }
                }
                Ok(Some(
                    lowering.builder.emit(Type::Null, Op::Const(Constant::Null)),
                ))
            },
        )?;
        let next = self
            .builder
            .emit(Type::Int, Op::Arith(Arith::Add, Item::Int, counter, one));
        self.builder.push(Some(counter), Op::Copy(next));
        self.builder.terminate(Terminator::Jump(header));
        self.builder.switch_to(preheader);
        let zero = self.int(0);
        let fresh = self
            .builder
            .emit(Type::vector(payload), Op::NewVector(payload, zero));
        let fresh = self.builder.emit(output_ty, Op::Relabel(fresh, output_ty));
        self.builder.push(Some(output), Op::Copy(fresh));
        self.builder.switch_to(exit);
        // The interpreter's list of no items has no item type, which a typed list cannot match.
        let kept_count = self.builder.emit(Type::Int, Op::Length(output));
        let zero = self.int(0);
        let empty = self.builder.emit(Type::Bool, Op::Same(kept_count, zero));
        self.fail_if(empty);
        Ok(output)
    }

    /// `Dict.get(dict, key)`: `:Ok(value)`, or `:Err(:KeyNotFound)`.
    unsafe fn dict_get(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [dict, key] = arguments.exactly(2)?[..] else {
            return Err(Refusal::new("a malformed `Dict.get`"));
        };
        let dict = self.value(dict)?;
        let key = self.value(key)?;
        let key = self.dict_key(dict, key)?;
        self.lookup_result(dict, key, "KeyNotFound")
    }

    /// `Dict.insert(dict, key, value)`, which updates a uniquely held dictionary in place.
    unsafe fn dict_insert(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [dict, key, value] = arguments.exactly(3)?[..] else {
            return Err(Refusal::new("a malformed `Dict.insert`"));
        };
        let dict = self.value(dict)?;
        let key = self.value(key)?;
        let value = self.value(value)?;
        let key = self.dict_key(dict, key)?;
        let dict_ty = self.builder.ty(dict);
        let Type::Dict { value: item, .. } = dict_ty else {
            return Err(Refusal::new(
                "a `Dict.insert` into something other than a dictionary",
            ));
        };
        // The interpreter checks the value against the dictionary's value type, and converts
        // nothing.
        if self.convert_scalar(value, item)? != value {
            return Err(Refusal::new("an insert of a value of another type"));
        }
        Ok(self.builder.emit(dict_ty, Op::DictInsert(dict, key, value)))
    }

    /// `Dict.to_list(dict)`: a list of `(key, value)` tuples in insertion order, built in a loop
    /// over the dictionary's keys and values.
    unsafe fn dict_to_list(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [dict] = arguments.exactly(1)?[..] else {
            return Err(Refusal::new("a malformed `Dict.to_list`"));
        };
        let dict = self.value(dict)?;
        let Type::Dict { key, value } = self.builder.ty(dict) else {
            return Err(Refusal::new(
                "a `Dict.to_list` of something other than a dictionary",
            ));
        };
        let (layout, _) =
            interp::records::constructor_layout(None, &[rvalues::nil(), rvalues::nil()], false)
                .ok_or(Refusal::new("a pair the interpreter rejects"))?;
        let shape = shapes::intern(layout, &[key, value])
            .ok_or(Refusal::new("a pair a box cannot hold"))?;
        let keys = self.builder.emit(
            Type::Vector {
                item: key,
                array: true,
            },
            Op::DictKeys(dict),
        );
        let values = self.builder.emit(
            Type::Vector {
                item: value,
                array: true,
            },
            Op::DictValues(dict),
        );
        let count = self.builder.emit(Type::Int, Op::Length(keys));
        let item = Item::Record(shape);
        let zero = self.int(0);
        let fresh = self
            .builder
            .emit(Type::vector(item), Op::NewVector(item, zero));
        let ty = Type::Vector { item, array: true };
        let empty = self.builder.emit(ty, Op::Relabel(fresh, ty));
        let pairs = self.builder.emit(ty, Op::Copy(empty));
        let counter = self.builder.local(Type::Int);
        let one = self.int(1);
        self.builder.push(Some(counter), Op::Copy(one));
        let header = self.loop_header();
        let past = self.builder.emit(
            Type::Lgl,
            Op::Compare(Compare::Gt, Item::Int, counter, count),
        );
        let past = self.builder.emit(Type::Bool, Op::Truth(past));
        let (body, exit) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: past,
            then: exit,
            otherwise: body,
        });
        self.builder.switch_to(body);
        let entry_key = self
            .builder
            .emit(Type::of_item(key), Op::Get(keys, counter));
        let entry_value = self
            .builder
            .emit(Type::of_item(value), Op::Get(values, counter));
        let pair = self.boxed(layout, false, &[entry_key, entry_value])?;
        let grown = self.builder.emit(ty, Op::Push(pairs, pair));
        self.builder.push(Some(pairs), Op::Copy(grown));
        let next = self
            .builder
            .emit(Type::Int, Op::Arith(Arith::Add, Item::Int, counter, one));
        self.builder.push(Some(counter), Op::Copy(next));
        self.builder.terminate(Terminator::Jump(header));
        self.builder.switch_to(exit);
        Ok(pairs)
    }

    /// The key of a lookup in `dict` as the dictionary holds keys.
    unsafe fn dict_key(&mut self, dict: Local, key: Local) -> Result<Local, Refusal> {
        let Type::Dict { key: item, .. } = self.builder.ty(dict) else {
            return Err(Refusal::new(
                "a lookup in something other than a dictionary",
            ));
        };
        self.convert_scalar(key, item)
    }

    /// `:Ok(value)` when `dict` holds `key`, else `:Err(:missing)`.
    unsafe fn lookup_result(
        &mut self,
        dict: Local,
        key: Local,
        missing: &str,
    ) -> Result<Local, Refusal> {
        let Type::Dict { value: item, .. } = self.builder.ty(dict) else {
            return Err(Refusal::new(
                "a lookup in something other than a dictionary",
            ));
        };
        let contains = self.builder.emit(Type::Bool, Op::DictContains(dict, key));
        let (found, absent, join) = (
            self.builder.block(),
            self.builder.block(),
            self.builder.block(),
        );
        self.builder.terminate(Terminator::Branch {
            condition: contains,
            then: found,
            otherwise: absent,
        });
        self.builder.switch_to(found);
        let value = self
            .builder
            .emit(Type::of_item(item), Op::DictGet(dict, key));
        let ok = self.tag("Ok", &[value])?;
        self.builder.switch_to(absent);
        let reason = self.tag(missing, &[])?;
        let err = self.tag("Err", &[reason])?;
        let Type::Tag(union) = joined(self.builder.ty(ok), self.builder.ty(err))
            .ok_or(Refusal::new("a lookup result"))?
        else {
            return Err(Refusal::new("a lookup result"));
        };
        let result = self.builder.local(Type::Tag(union));
        self.builder.push(Some(result), Op::Copy(err));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(found);
        self.builder.push(Some(result), Op::Copy(ok));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(join);
        Ok(result)
    }

    /// `Dict.update(dict, key, f)`: `f` gets `:Ok(value)` or `:Err(:Missing)`, and an `:Ok` it
    /// gives sets the key while an `:Err` removes it.
    unsafe fn dict_update(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [dict, key, function] = arguments.exactly(3)?[..] else {
            return Err(Refusal::new("a malformed `Dict.update`"));
        };
        let dict = self.value(dict)?;
        let key = self.value(key)?;
        let callable = self.function_argument(function)?;
        let key = self.dict_key(dict, key)?;
        let dict_ty = self.builder.ty(dict);
        let Type::Dict { value: item, .. } = dict_ty else {
            return Err(Refusal::new(
                "a `Dict.update` of something other than a dictionary",
            ));
        };
        let current = self.lookup_result(dict, key, "Missing")?;
        let answer = self
            .apply(callable, &[current])?
            .ok_or(Refusal::new("an update that never completes"))?;
        let (ok, err) = (tag_layout("Ok", 1)?, tag_layout("Err", 1)?);
        let (candidates, exhaustive) = self.shapes_of(self.builder.ty(answer), &[ok, err], &[])?;
        let updated = self.switch_shapes(
            answer,
            &candidates,
            exhaustive,
            None,
            &mut |lowering, shape| {
                let layout = shape.map(|shape| shapes::shape(shape).layout);
                match (shape, layout) {
                    (Some(shape), Some(layout)) if layout == ok => {
                        let value = lowering.field(answer, shape, 0)?;
                        // A dictionary's values all have its value type, which `Dict.update`
                        // checks rather than converts, as `Dict.insert` does.
                        if lowering.convert_scalar(value, item)? != value {
                            return Err(Refusal::new("an update to a value of another type"));
                        }
                        Ok(Some(
                            lowering
                                .builder
                                .emit(dict_ty, Op::DictInsert(dict, key, value)),
                        ))
                    }
                    (_, Some(layout)) if layout == err => Ok(Some(
                        lowering.builder.emit(dict_ty, Op::DictRemove(dict, key)),
                    )),
                    _ => {
                        lowering.builder.terminate(Terminator::Fail);
                        let unreachable = lowering.builder.block();
                        lowering.builder.switch_to(unreachable);
                        Ok(None)
                    }
                }
            },
        )?;
        let updated = updated.ok_or(Refusal::new("an update that never completes"))?;
        self.usable(updated)?;
        Ok(updated)
    }

    /// `Str.to_i64(string)` or `Str.to_f64(string)`: `:Ok(number)`, or `:Err(:InvalidNumStr)`.
    unsafe fn parse_number(&mut self, i64: bool, arguments: &Arguments) -> Result<Local, Refusal> {
        let [string] = arguments.exactly(1)?[..] else {
            return Err(Refusal::new("a malformed `Str.to_i64`"));
        };
        let string = self.value(string)?;
        if self.builder.ty(string) != Type::Str {
            return Err(Refusal::new(
                "a `Str.to_i64` of something other than a string",
            ));
        }
        let ty = if i64 { Type::I64 } else { Type::Dbl };
        let parses = self.builder.emit(Type::Bool, Op::Parses(i64, string));
        let (valid, invalid, join) = (
            self.builder.block(),
            self.builder.block(),
            self.builder.block(),
        );
        self.builder.terminate(Terminator::Branch {
            condition: parses,
            then: valid,
            otherwise: invalid,
        });
        self.builder.switch_to(valid);
        let number = self.builder.emit(ty, Op::Parse(string));
        let ok = self.tag("Ok", &[number])?;
        self.builder.switch_to(invalid);
        let reason = self.tag("InvalidNumStr", &[])?;
        let err = self.tag("Err", &[reason])?;
        let Some(Type::Tag(union)) = joined(self.builder.ty(ok), self.builder.ty(err)) else {
            return Err(Refusal::new("a parse result"));
        };
        let result = self.builder.local(Type::Tag(union));
        self.builder.push(Some(result), Op::Copy(err));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(valid);
        self.builder.push(Some(result), Op::Copy(ok));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(join);
        Ok(result)
    }

    unsafe fn paste(&mut self, paste0: bool, arguments: &Arguments) -> Result<Local, Refusal> {
        if !arguments.only_names(&["sep", "collapse"]) {
            return Err(Refusal::new("a `paste` with unsupported arguments"));
        }
        let separator = match (paste0, arguments.named("sep")) {
            (true, Some(_)) => return Err(Refusal::new("a `paste0` with `sep`")),
            (true, None) => String::new(),
            (false, None) => String::from(" "),
            (false, Some(separator)) => {
                constant_string(separator).ok_or(Refusal::new("a computed `paste` separator"))?
            }
        };
        let collapse = match arguments.named("collapse") {
            Some(collapse) => {
                Some(constant_string(collapse).ok_or(Refusal::new("a computed `paste` collapse"))?)
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
            return Err(Refusal::new("a `paste` of nothing"));
        }
        let separator = self.string(&separator);
        let pasted = if vectors || collapse.is_some() {
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
                let item = ty
                    .item()
                    .ok_or(Refusal::new("a value that is not a scalar"))?;
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
            _ => Err(Refusal::new(
                "a value `paste` cannot turn into a string here",
            )),
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
            _ => Err(Refusal::new("a value that is not a number")),
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
                    (Item::Str, _) => return Err(Refusal::new("a reduction of strings")),
                    (_, Reduce::Mean | Reduce::Prod) => (Item::Dbl, Type::Dbl),
                    (Item::Dbl, _) => (Item::Dbl, Type::Dbl),
                    (Item::Int | Item::Lgl, Reduce::Sum) => (item, Type::Int),
                    (Item::Int, Reduce::Max | Reduce::Min) => (Item::Int, Type::Int),
                    _ => return Err(Refusal::new("a reduction the typed core does not know")),
                };
                let value = if item == Item::Dbl {
                    self.as_double(value)?
                } else {
                    value
                };
                Ok(self.builder.emit(result, Op::Reduce(reduce, item, value)))
            }
            _ => Err(Refusal::new("a reduction of something other than numbers")),
        }
    }

    unsafe fn math(&mut self, op: Math, value: Local) -> Result<Local, Refusal> {
        let ty = self.builder.ty(value);
        let keeps_integers = matches!(op, Math::Neg | Math::Abs);
        match ty {
            Type::I64 if keeps_integers => {
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
            _ => Err(Refusal::new(
                "a math function of something other than numbers",
            )),
        }
    }

    /// `Num.min(a, b)` or `Num.max(a, b)`. With an `I64` both are `I64`s; two R integers stay
    /// integers, and otherwise both are doubles. The first wins a tie, and `NA` or `NaN` is an
    /// error the interpreter raises.
    /// `Num.compare(a, b)`: `:LT`, `:EQ` or `:GT`. Two `I64`s, or an `I64` and an R number that
    /// converts exactly, compare as integers, and two R numbers as doubles; the library refuses
    /// `NaN` and `NA`.
    unsafe fn compare_numbers(&mut self, a: Local, b: Local) -> Result<Local, Refusal> {
        let item = match (self.builder.ty(a), self.builder.ty(b)) {
            (Type::I64, _) | (_, Type::I64) => Item::I64,
            (Type::Int | Type::Dbl, Type::Int | Type::Dbl) => Item::Dbl,
            _ => {
                return Err(Refusal::new(
                    "a `Num.compare` of something other than numbers",
                ));
            }
        };
        let (a, b) = if item == Item::I64 {
            (self.to_i64(a)?, self.to_i64(b)?)
        } else {
            let (a, b) = (self.as_double(a)?, self.as_double(b)?);
            self.fail_if_nan(a);
            self.fail_if_nan(b);
            (a, b)
        };
        let less = self.tag("LT", &[])?;
        let equal = self.tag("EQ", &[])?;
        let greater = self.tag("GT", &[])?;
        let union = [less, equal, greater]
            .iter()
            .try_fold(None, |union: Option<Type>, &tag| {
                let ty = self.builder.ty(tag);
                match union {
                    None => Some(Some(ty)),
                    Some(union) => joined(union, ty).map(Some),
                }
            })
            .flatten()
            .ok_or(Refusal::new("a comparison result"))?;
        let result = self.builder.local(union);
        let join = self.builder.block();
        for (condition, tag) in [(Compare::Lt, less), (Compare::Gt, greater)] {
            let test = self
                .builder
                .emit(Type::Lgl, Op::Compare(condition, item, a, b));
            let test = self.builder.emit(Type::Bool, Op::Truth(test));
            let (taken, next) = (self.builder.block(), self.builder.block());
            self.builder.terminate(Terminator::Branch {
                condition: test,
                then: taken,
                otherwise: next,
            });
            self.builder.switch_to(taken);
            self.builder.push(Some(result), Op::Copy(tag));
            self.builder.terminate(Terminator::Jump(join));
            self.builder.switch_to(next);
        }
        self.builder.push(Some(result), Op::Copy(equal));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(join);
        Ok(result)
    }

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
            _ => Err(Refusal::new("a `Num.min` of something other than numbers")),
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
                    _ => Err(Refusal::new("list arithmetic between different item types")),
                }
            };
            let valid = match item {
                Item::Dbl => true,
                Item::I64 => matches!(op, Arith::Add | Arith::Sub | Arith::Mul),
                _ => false,
            };
            if !valid {
                return Err(Refusal::new(
                    "list arithmetic the typed core does not compile",
                ));
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
                return Err(Refusal::new("a `/` of an I64, which is an error"));
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
            return Err(Refusal::new("arithmetic on something other than numbers"));
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
        let index = u8::try_from(nodes.len())
            .map_err(|_| Refusal::new("an arithmetic expression too large"))?;
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
                    return Err(Refusal::new("an arithmetic tree out of order"));
                };
                values[index] = Some(self.arith(op, a, b)?);
            }
        }
        values[root]
            .map(|value| (value, false))
            .ok_or(Refusal::new("an empty arithmetic tree"))
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
        let i64_list = |ty: Type| {
            matches!(
                ty,
                Type::Vector {
                    item: Item::I64,
                    ..
                }
            )
        };
        // A `List<I64>` compares with another or with a number as `I64`s, giving a
        // `List<Bool>`.
        if i64_list(ta) || i64_list(tb) {
            let a = if i64_list(ta) { a } else { self.to_i64(a)? };
            let b = if i64_list(tb) { b } else { self.to_i64(b)? };
            let result = Type::Vector {
                item: Item::Lgl,
                array: true,
            };
            return Ok(self
                .builder
                .emit(result, Op::VectorCompare(op, Item::I64, a, b)));
        }
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
            return Err(Refusal::new(
                "a comparison of something other than scalars or vectors",
            ));
        };
        let item = match (ia, ib) {
            (Item::Str, Item::Str) => Item::Str,
            (Item::Str, _) | (_, Item::Str) => {
                return Err(Refusal::new("a comparison of a string and a number"));
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
            // A comparison with a ry list is a ry list.
            let list = |ty: Type| matches!(ty, Type::Vector { array: true, .. });
            let result = Type::Vector {
                item: Item::Lgl,
                array: list(ta) || list(tb),
            };
            Ok(self.builder.emit(result, Op::VectorCompare(op, item, a, b)))
        } else {
            Ok(self.builder.emit(Type::Lgl, Op::Compare(op, item, a, b)))
        }
    }
}

/// The layout of the tag `:name(...)` with `count` positional fields.
unsafe fn tag_layout(name: &str, count: u32) -> Result<u32, Refusal> {
    let symbol = rvalues::install(name).map_err(|_| Refusal::new("a tag name"))?;
    let layouts = &mut interp::heap().layouts;
    let tag = layouts.intern_tag(symbol);
    Ok(layouts.intern(Layout {
        tag: Some(tag),
        fields: Fields::Positional(count),
    }))
}

/// A dictionary's type from its annotation, such as `Dict<Str, F64>`.
fn dict_type(annotation: &str) -> Option<Type> {
    let inner = annotation.trim().strip_prefix("Dict<")?.strip_suffix('>')?;
    let (key, value) = inner.split_once(',')?;
    let item = |name: &str| match name.trim() {
        "Str" => Some(Item::Str),
        "I64" => Some(Item::I64),
        "F64" => Some(Item::Dbl),
        _ => None,
    };
    Some(Type::Dict {
        key: item(key)?,
        value: item(value)?,
    })
}

/// What a `match` pattern of a tuple, record or tag names: a layout and each item's slot, or
/// for `:Name(..)` only the tag.
enum PatternLayout {
    Exact(u32, Vec<usize>),
    Tag(u32),
}

/// The layout a constructor pattern `.ry_tag(...)`, `.ry_record(...)` or `.ry_tuple(...)`
/// tests, with its items.
unsafe fn pattern_layout(pattern: Sexp) -> Result<(PatternLayout, Vec<(Sexp, Sexp)>), Refusal> {
    let head = rvalues::symbol_name(pattern.car());
    let items = rvalues::pairlist_items(pattern.cdr());
    let (tag, payload) = if head == ".ry_tag" {
        let Some((&(name_tag, name), payload)) = items.split_first() else {
            return Err(Refusal::new("a tag pattern without a name"));
        };
        if name_tag != rvalues::nil() {
            return Err(Refusal::new("a tag pattern whose name is an argument"));
        }
        (Some(name), payload.to_vec())
    } else {
        (None, items)
    };
    let rest = |item: Sexp| item.kind() == SexpType::Symbol && rvalues::symbol_name(item) == "..";
    if let (Some(name), [(item_tag, item)]) = (tag, &payload[..])
        && *item_tag == rvalues::nil()
        && rest(*item)
    {
        let text = rvalues::string_at(name, 0).ok_or(Refusal::new("a tag pattern's name"))?;
        let symbol = rvalues::install(&text).map_err(|_| Refusal::new("a tag name"))?;
        let tag = interp::heap().layouts.intern_tag(symbol);
        return Ok((PatternLayout::Tag(tag), payload));
    }
    if payload.iter().any(|&(_, item)| rest(item)) {
        return Err(Refusal::new("a pattern with `..` among its fields"));
    }
    let names: Vec<Sexp> = payload.iter().map(|&(tag, _)| tag).collect();
    let (layout, slots) = interp::records::constructor_layout(tag, &names, head == ".ry_record")
        .ok_or(Refusal::new("a pattern the interpreter rejects"))?;
    Ok((PatternLayout::Exact(layout, slots), payload))
}

/// Adds the layouts `pattern` names, and the tags it names with any payload, which a `match` over
/// any tag tests for.
unsafe fn pattern_layouts(
    pattern: Sexp,
    layouts: &mut SmallVec<[u32; 4]>,
    tags: &mut SmallVec<[u32; 4]>,
) -> Result<(), Refusal> {
    if pattern.kind() != SexpType::Language || pattern.car().kind() != SexpType::Symbol {
        return Ok(());
    }
    match &*rvalues::symbol_name(pattern.car()) {
        "(" | "|" => {
            for (_, inner) in rvalues::pairlist_items(pattern.cdr()) {
                pattern_layouts(inner, layouts, tags)?;
            }
            Ok(())
        }
        ".ry_tag" | ".ry_record" | ".ry_tuple" => match pattern_layout(pattern)?.0 {
            PatternLayout::Exact(layout, _) => {
                if !layouts.contains(&layout) {
                    layouts.push(layout);
                }
                Ok(())
            }
            PatternLayout::Tag(tag) => {
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
                Ok(())
            }
        },
        _ => Ok(()),
    }
}

/// Whether `pattern` matches a box of `shape`, or with `None` a box of a shape no pattern
/// names, adding the names it binds: to the whole value, or to a field by index.
unsafe fn pattern_matches(
    pattern: Sexp,
    shape: Option<u32>,
    bindings: &mut SmallVec<[(Sexp, Option<usize>); 4]>,
) -> Result<bool, Refusal> {
    match pattern.kind() {
        SexpType::Symbol => match &*rvalues::symbol_name(pattern) {
            "_" => Ok(true),
            "T" | "F" | ".." | "..." | "" => Err(Refusal::new(
                "a `match` pattern the typed core does not know",
            )),
            _ => {
                bindings.push((pattern, None));
                Ok(true)
            }
        },
        SexpType::Language if pattern.car().kind() == SexpType::Symbol => {
            let head = rvalues::symbol_name(pattern.car());
            let items = rvalues::pairlist_items(pattern.cdr());
            match (&*head, &items[..]) {
                ("(", &[(_, inner)]) => pattern_matches(inner, shape, bindings),
                ("|", &[(_, left), (_, right)]) => {
                    let before = bindings.len();
                    let found = pattern_matches(left, shape, bindings)?
                        || pattern_matches(right, shape, bindings)?;
                    if bindings.len() != before {
                        return Err(Refusal::new("an alternative pattern that binds names"));
                    }
                    Ok(found)
                }
                (".ry_tag" | ".ry_record" | ".ry_tuple", _) => {
                    let (layout, payload) = pattern_layout(pattern)?;
                    let Some(shape) = shape else {
                        return Ok(false);
                    };
                    let found = shapes::shape(shape).layout;
                    match layout {
                        PatternLayout::Tag(tag) => Ok(interp::heap()
                            .layouts
                            .get(found)
                            .is_some_and(|layout| layout.tag == Some(tag))),
                        PatternLayout::Exact(layout, slots) => {
                            if layout != found {
                                return Ok(false);
                            }
                            for (&(_, item), &slot) in payload.iter().zip(&slots) {
                                field_pattern(item, slot, bindings)?;
                            }
                            Ok(true)
                        }
                    }
                }
                _ => Err(Refusal::new(
                    "a `match` pattern the typed core does not know",
                )),
            }
        }
        _ => Err(Refusal::new(
            "a `match` pattern the typed core does not know",
        )),
    }
}

/// A field's pattern, which matches any value: `_`, or a name it binds.
unsafe fn field_pattern(
    pattern: Sexp,
    slot: usize,
    bindings: &mut SmallVec<[(Sexp, Option<usize>); 4]>,
) -> Result<(), Refusal> {
    match pattern.kind() {
        SexpType::Symbol => match &*rvalues::symbol_name(pattern) {
            "_" => Ok(()),
            "T" | "F" | ".." | "..." | "" => {
                Err(Refusal::new("a field pattern the typed core does not know"))
            }
            _ => {
                bindings.push((pattern, Some(slot)));
                Ok(())
            }
        },
        SexpType::Language
            if pattern.car().kind() == SexpType::Symbol
                && rvalues::symbol_name(pattern.car()) == "(" =>
        {
            match rvalues::pairlist_items(pattern.cdr())[..] {
                [(_, inner)] => field_pattern(inner, slot, bindings),
                _ => Err(Refusal::new("a malformed pattern")),
            }
        }
        _ => Err(Refusal::new("a nested `match` pattern")),
    }
}

/// The type both of two values' types fit: that type, or the union of two tags.
fn joined(a: Type, b: Type) -> Option<Type> {
    match (a, b) {
        (Type::Tag(a), Type::Tag(b)) => Some(Type::Tag(shapes::join(a, b))),
        // A `Bool` is a logical that is never `NA`, held in the same word.
        (Type::Bool, Type::Lgl) | (Type::Lgl, Type::Bool) => Some(Type::Lgl),
        (a, b) => (a == b).then_some(a),
    }
}

/// The R storage type of `ty` and whether it is an Array, as an inlined local function's checks
/// see it. `None` for a list of boxes or a value with no R storage.
fn inline_storage(ty: Type) -> Option<(SexpType, bool)> {
    match ty {
        Type::Dict { .. } => Some((SexpType::Record, false)),
        Type::List(_) => None,
        _ => crate::storage(ty),
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

/// Collects the `match` calls whose value is the value of `expression`: `expression` itself,
/// the last expression of a block, the branches of an `if` and the bodies of a `match`'s arms.
unsafe fn tail_matches(expression: Sexp, tails: &mut SmallVec<[Sexp; 2]>) {
    if expression.kind() != SexpType::Language || expression.car().kind() != SexpType::Symbol {
        return;
    }
    let args = rvalues::pairlist_items(expression.cdr());
    match &*rvalues::symbol_name(expression.car()) {
        "{" | "(" => {
            if let Some(&(_, last)) = args.last() {
                tail_matches(last, tails);
            }
        }
        "if" => {
            for &(_, branch) in args.iter().skip(1) {
                tail_matches(branch, tails);
            }
        }
        ".ry_match" => {
            tails.push(expression);
            for &(_, arm) in args.iter().skip(1) {
                if arm.kind() == SexpType::Language {
                    if let Some(&(_, body)) = rvalues::pairlist_items(arm.cdr()).last() {
                        tail_matches(body, tails);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Whether `function` is base R's or the standard library's own function of this name.
/// The refusal of a call of the R function `head` whose value typed code uses directly.
unsafe fn r_value_refusal(head: Sexp) -> Refusal {
    Refusal(Cow::Owned(format!(
        "it uses the value of R's `{}` directly, and typed code calls R only as a statement or \
         inside `Num.from_r`, `Str.from_r`, `Bool.from_r` or `List.from_r`, which give the value \
         a ry type",
        interp::deparse::deparse_first_line(head)
    )))
}

/// The type a conversion of what an R call returns gives: fixed for `Str.from_r` and
/// `Bool.from_r`, and for `Num.from_r` and `List.from_r` the type the conversion gave when the
/// interpreter last ran it.
unsafe fn conversion_type(conversion: Conversion) -> Result<Type, Refusal> {
    let name = rvalues::symbol_name(conversion.call.car());
    let observed = || interp::numbers::observed_conversion(conversion.call);
    let awaiting = || {
        AWAITED.set(Some(Awaited::Conversion(conversion.call)));
        Refusal(Cow::Owned(format!(
            "{AWAITING}{}` once, which gives the type the conversion returns",
            interp::deparse::deparse_first_line(conversion.call)
        )))
    };
    let list = |item| Type::Vector { item, array: true };
    match &*name {
        "Str.from_r" => Ok(Type::Str),
        "Bool.from_r" => Ok(Type::Lgl),
        "Num.from_r" => match observed() {
            Some(SexpType::Real) => Ok(Type::Dbl),
            Some(SexpType::I64) => Ok(Type::I64),
            Some(_) => Err(Refusal::new("a `Num.from_r` of an unexpected type")),
            None => Err(awaiting()),
        },
        _ => match observed() {
            Some(SexpType::Real) => Ok(list(Item::Dbl)),
            Some(SexpType::Logical) => Ok(list(Item::Lgl)),
            Some(SexpType::String) => Ok(list(Item::Str)),
            Some(SexpType::I64) => Ok(list(Item::I64)),
            Some(_) => Err(Refusal::new(
                "a `List.from_r` of an R list, whose items typed code cannot take from R yet",
            )),
            None => Err(awaiting()),
        },
    }
}

/// The R function `package::name` names, when `head` is that form: an error until the package's
/// namespace is loaded, and `None` when it exports no function of that name.
unsafe fn namespaced_function(head: Sexp) -> Option<Result<Speculation, Refusal>> {
    if head.kind() != SexpType::Language
        || head.car().kind() != SexpType::Symbol
        || rvalues::symbol_name(head.car()) != "::"
    {
        return None;
    }
    let [(_, package), (_, symbol)] = rvalues::pairlist_items(head.cdr())[..] else {
        return None;
    };
    if package.kind() != SexpType::Symbol || symbol.kind() != SexpType::Symbol {
        return None;
    }
    // Resolving a namespace that is not loaded would load it, which only running the code may do.
    let Some(namespace) = loaded_namespace(package) else {
        AWAITED.set(Some(Awaited::Namespace(package)));
        return Some(Err(Refusal(Cow::Owned(format!(
            "{AWAITING}{}` once, which loads the namespace `{}`",
            interp::deparse::deparse_first_line(head),
            rvalues::symbol_name(package)
        )))));
    };
    // Forcing a lazy-load promise is the interpreter's to do when the code runs.
    if let Some(cell) = interp::heap().frame_cell(namespace, symbol)
        && cell.cdr().kind() == SexpType::Promise
        && cell.cdr().car() == rvalues::unbound()
    {
        AWAITED.set(Some(Awaited::Promise(cell.cdr())));
        return Some(Err(Refusal(Cow::Owned(format!(
            "{AWAITING}{}` once, which loads the function",
            interp::deparse::deparse_first_line(head)
        )))));
    }
    let speculation = resolve_function(symbol, namespace)?;
    let (exported, _) = interp::envir::namespace_binding(package, symbol, true).ok()?;
    (exported == speculation.function).then_some(Ok(speculation))
}

/// The namespace of the package named `package`, when it is loaded.
unsafe fn loaded_namespace(package: Sexp) -> Option<Sexp> {
    let registry = interp::heap().known().namespace_registry;
    let namespace = interp::eval::find_var_in_frame(package, registry).ok()?;
    (namespace.kind() == SexpType::Environment).then_some(namespace)
}

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
