//! Native functions: a closure whose body only computes a scalar from scalars and plain
//! vectors, compiled to machine code that takes its scalar arguments unboxed and its vectors as
//! their objects, and returns its value unboxed, with no frame, no promises and no annotation
//! checks. The body may loop (`for` over a vector, `seq_len`, `seq_along` or `a:b`, `while`,
//! `repeat`, `break`, `next`) and read vectors (`length`, and `[[` or `[` with a scalar index).
//!
//! Compiled code calls one in place of the closure when it knows the arguments' types (see
//! `Action::NativeCall`), and a native function calls the native functions it reaches,
//! itself included, the same way. A recursive scalar function such as
//! `fib <- function(n) if (n < 2) n else fib(n - 1) + fib(n - 2)` then runs entirely in
//! machine code.
//!
//! Skipping the frame is invisible because the body can observe nothing about its caller: it
//! reads only its parameters and its own locals, calls only base arithmetic and other such
//! functions, and has no effect. It allocates nothing either, so the vectors it reads, which
//! its caller roots, cannot move or die while it runs. Its arguments are evaluated before the call, which lazy
//! evaluation could only notice through an effect or an error, and the body has neither: every
//! operation that would warn or raise an error in R (an `NA` condition, an integer overflow, a
//! `NaN` from `sqrt`, a call nested deeper than R allows, an index out of range, a pending
//! interrupt in a loop) makes the function return [`FAILED`] instead. Nothing anyone can see has happened by then, so the caller runs the call again in
//! the interpreter, which reports the problem as R would.
//!
//! The annotation checks of a typed closure are decided when it compiles, from the argument
//! types the native function is specialized to: a function whose checks would fail for them is
//! never compiled, so its calls skip the checks the types already prove.

use {
    crate::{
        analysis::{Refusal, Speculation, Ty, base_builtin, constant_type, resolve_function},
        codegen, runtime,
    },
    cranelift_codegen::{
        ir::{
            AbiParam, FuncRef, InstBuilder, MemFlagsData, Signature, StackSlotData, StackSlotKind,
            Type, Value,
            condcodes::{FloatCC, IntCC},
            immediates::Ieee64,
            types::{F64, I32, I64},
        },
        isa::CallConv,
    },
    cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable},
    cranelift_module::Module,
    heap::{
        object::{
            INTEGER_PREFIX, LOGICAL_PREFIX, NA_INTEGER, NA_REAL_BITS, RECORD_DATA_OFFSET,
            RECORD_LAYOUT_OFFSET, RECORD_UNBOXED_OFFSET, Sexp, SexpType, UNBOXED_SLOTS,
        },
        records::{Fields, slot_kind},
    },
    interp::{annotations, bytecode, records, values},
    rustc_hash::{FxHashMap, FxHashSet},
    smallvec::SmallVec,
    std::{
        cell::{Cell, RefCell},
        sync::atomic::AtomicBool,
    },
};

/// The status a native function returns beside its value when it ran to the end.
pub const OK: i64 = 0;
/// The status a native function returns where R would warn, raise an error or nest too deeply:
/// the caller runs the call again in the interpreter.
pub const FAILED: i64 = 1;

/// A compiled native function. Its code lives for the session, like all compiled code, and it
/// keeps its closure rooted, so no other closure can take the closure's address as a key.
#[derive(Debug)]
pub struct Native {
    /// The machine code. It takes the number of calls that may still nest and then each
    /// argument (`i32` for an integer or a logical, `f64` for a double), and returns a status
    /// ([`OK`] or [`FAILED`]) and the value.
    pub address: usize,
    pub params: SmallVec<[Ty; 4]>,
    pub result: Ty,
    /// The function lookups the code assumes, its callees' included: a caller compiled against
    /// it checks them as its own.
    pub speculations: Vec<Speculation>,
    /// Whether the code allocates records. It roots each one on the handle stack and releases
    /// them all when it returns, except the record it returns, which it leaves rooted.
    pub allocates: bool,
}

/// The machine signature of a native function with these argument and result types.
pub fn signature(params: &[Ty], result: Ty, call_conv: CallConv) -> Signature {
    let mut signature = Signature::new(call_conv);
    signature.params.push(AbiParam::new(I64));
    signature
        .params
        .extend(params.iter().map(|&ty| AbiParam::new(machine_type(ty))));
    signature.returns.push(AbiParam::new(I64));
    signature.returns.push(AbiParam::new(machine_type(result)));
    signature
}

type Key = (Sexp, SmallVec<[Ty; 4]>);

thread_local! {
    /// Native functions compiled or refused, by closure and argument types.
    static NATIVES: RefCell<FxHashMap<Key, Option<&'static Native>>> =
        RefCell::new(FxHashMap::default());
    /// The functions being compiled, so a cycle through other specializations stops.
    static COMPILING: RefCell<FxHashSet<Key>> = RefCell::new(FxHashSet::default());
    /// How many native functions exist, which refusals in `NATIVES` do not count toward.
    static COMPILED: Cell<usize> = const { Cell::new(0) };
}

/// How many native functions a session compiles at most, since each keeps its closure alive.
const MAX_NATIVES: usize = 4096;

/// How many refusals the cache keeps before it forgets them. A refusal is keyed by a closure
/// that is not rooted, so its address may be reused by another closure that deserves a try.
const MAX_REFUSALS: usize = 65536;

/// How deep a body's expressions may nest, which bounds the compiler's own recursion.
const MAX_NESTING: usize = 200;
/// The most calls and operators a callee's body may hold to be compiled into its caller.
const MAX_INLINE_SIZE: usize = 24;
/// How deeply inlined bodies may nest.
const MAX_INLINE_DEPTH: usize = 3;

/// The native function for calling `function` with unboxed arguments of types `params`,
/// compiling it on first use. `None` when the closure is not a scalar function of them.
pub unsafe fn native_for(function: Sexp, params: &[Ty]) -> Option<&'static Native> {
    let key: Key = (function, SmallVec::from_slice(params));
    match NATIVES.with(|natives| natives.borrow().get(&key).copied()) {
        // A function the code calls was rebound since it compiled: compile it again.
        Some(Some(native)) if !still_resolves(native) => {}
        Some(found) => return found,
        None => {}
    }
    if COMPILED.with(Cell::get) >= MAX_NATIVES
        || !COMPILING.with(|compiling| compiling.borrow_mut().insert(key.clone()))
    {
        return None;
    }
    let compiled = compile(function, params);
    COMPILING.with(|compiling| compiling.borrow_mut().remove(&key));
    let compiled = match compiled {
        Ok(native) => {
            interp::heap().add_global_root(function);
            COMPILED.with(|count| count.set(count.get() + 1));
            Some(&*Box::leak(Box::new(native)))
        }
        Err(Refusal(why)) => {
            if crate::logging() {
                eprintln!("jit: no native function for {params:?}: {why}");
            }
            None
        }
    };
    NATIVES.with(|natives| {
        let mut natives = natives.borrow_mut();
        if compiled.is_none() && natives.len() - COMPILED.with(Cell::get) >= MAX_REFUSALS {
            natives.retain(|_, native| native.is_some());
        }
        natives.insert(key, compiled);
    });
    compiled
}

/// Whether `function` has a native function for some argument types.
pub unsafe fn has_native(function: Sexp) -> bool {
    NATIVES.with_borrow(|natives| {
        natives
            .iter()
            .any(|((closure, _), native)| *closure == function && native.is_some())
    })
}

/// Whether every function lookup `native` assumes still finds the same function.
unsafe fn still_resolves(native: &Native) -> bool {
    native.speculations.iter().all(|assumed| {
        resolve_function(assumed.symbol, assumed.env)
            .is_some_and(|now| now.cell == assumed.cell && now.function == assumed.function)
    })
}

unsafe fn compile(function: Sexp, params: &[Ty]) -> Result<Native, Refusal> {
    let formals = values::pairlist_items(function.car());
    if formals.len() != params.len()
        || formals
            .iter()
            .any(|&(symbol, _)| symbol == values::symbols().dots)
    {
        return Err(Refusal(
            "the call does not supply every formal positionally",
        ));
    }
    if !params
        .iter()
        .all(|ty| ty.is_unboxed() || ty.is_vector() || ty.is_record())
    {
        return Err(Refusal(
            "an argument is neither a scalar, a plain vector nor a record",
        ));
    }
    let storages: SmallVec<[Option<SexpType>; 4]> =
        params.iter().map(|&ty| storage_of(ty)).collect();
    if matches!(
        annotations::entry_for(function, &storages),
        annotations::Entry::Unchecked
    ) {
        return Err(Refusal("the argument types fail the closure's annotations"));
    }
    let body = bytecode::closure_body(function);
    if tail_invisible(body) {
        return Err(Refusal("the body's value may be invisible"));
    }
    let parameters: SmallVec<[(Sexp, Ty); 4]> = formals
        .iter()
        .zip(params)
        .map(|(&(symbol, _), &ty)| (symbol, ty))
        .collect();
    // A recursive call has the function's own result type, which is what the body computes
    // once that type is known. The first walk types a recursive call as what the `return`s
    // before it pass back, if any, and each further walk assumes the type the one before
    // found, until one confirms its assumption: a tagged union's branches widen a variant's
    // record to any record, and nothing widens further.
    let mut first = Walker::new(function, &parameters, None);
    let mut guess = first.walk(None, body)?.kind.known()?;
    let mut confirmed = None;
    for _ in 0..3 {
        let mut walker = Walker::new(function, &parameters, Some(guess));
        let found = walker.walk(None, body)?.kind.known()?;
        if found == guess {
            confirmed = Some(walker);
            break;
        }
        guess = found;
    }
    let second = confirmed.ok_or(Refusal("the result type depends on the recursion"))?;
    if !(guess.is_unboxed() || guess.is_record()) {
        return Err(Refusal("a result that is neither a scalar nor a record"));
    }
    let storage = storage_of(guess).ok_or(Refusal("a result without a storage type"))?;
    if !annotations::return_admits(function, storage) {
        return Err(Refusal(
            "the result type fails the closure's return annotation",
        ));
    }
    let speculations = second.speculations;
    let allocates = second.allocates;
    let started = std::time::Instant::now();
    let (address, size) = codegen::with_module(|module| {
        let call_conv = module.isa().default_call_conv();
        let id = module
            .declare_anonymous_function(&signature(params, guess, call_conv))
            .map_err(|_| Refusal("Cranelift rejected the function declaration"))?;
        let mut context = module.make_context();
        context.func.signature = signature(params, guess, call_conv);
        let own = module.declare_func_in_func(id, &mut context.func);
        let frontend_config = module.isa().frontend_config();
        let mut function_context = FunctionBuilderContext::new();
        {
            let builder = FunctionBuilder::new(&mut context.func, &mut function_context);
            let mut walker = Walker::new(function, &parameters, Some(guess));
            walker.allocates = allocates;
            let mut builder = emit(builder, call_conv, own, &mut walker, body)?;
            builder.seal_all_blocks();
            builder.finalize(frontend_config);
        }
        if std::env::var_os("RY_JIT_DUMP").is_some() {
            eprintln!("{}", context.func.display());
        }
        module
            .define_function(id, &mut context)
            .map_err(|_| Refusal("Cranelift rejected the function"))?;
        let size = codegen::code_size(&context);
        module.clear_context(&mut context);
        module
            .finalize_definitions()
            .map_err(|_| Refusal("Cranelift could not finalize the code"))?;
        Ok((module.get_finalized_function(id) as usize, size))
    })?;
    if crate::logging() {
        eprintln!(
            "jit: native function for {params:?} -> {guess:?} in {} us, {size} bytes",
            started.elapsed().as_micros()
        );
    }
    Ok(Native {
        address,
        params: SmallVec::from_slice(params),
        result: guess,
        speculations,
        allocates,
    })
}

fn machine_type(ty: Ty) -> Type {
    match ty {
        Ty::Dbl => F64,
        _ if ty.is_vector() || ty.is_record() => I64,
        _ => I32,
    }
}

/// The storage type the annotation checks test a value of this type against: a vector shares
/// its elements'.
fn storage_of(ty: Ty) -> Option<SexpType> {
    if ty.is_record() {
        Some(SexpType::Record)
    } else {
        ty.element().storage()
    }
}

/// The type a native function speculates a field holds, from the kinds every value of the
/// layout has held in that slot. `None` for a slot no value has filled yet.
fn field_type(layout: u32, index: usize) -> Result<Option<Ty>, Refusal> {
    Ok(Some(
        match interp::heap().layouts.slot_kinds(layout, index) {
            0 => return Ok(None),
            slot_kind::DOUBLE => Ty::Dbl,
            slot_kind::INTEGER => Ty::Int,
            slot_kind::LOGICAL => Ty::Lgl,
            slot_kind::RECORD => Ty::Obj,
            _ => {
                return Err(Refusal(
                    "a field that holds values of different types, or not scalars or records",
                ));
            }
        },
    ))
}

/// The slot-profile bit of a value of type `ty`.
fn kind_bit(ty: Ty) -> u8 {
    match ty {
        Ty::Dbl => slot_kind::DOUBLE,
        Ty::Int => slot_kind::INTEGER,
        Ty::Lgl => slot_kind::LOGICAL,
        Ty::Rec(_) | Ty::Obj => slot_kind::RECORD,
        _ => slot_kind::OTHER,
    }
}

/// Whether the value of `expression` may be invisible, as an assignment's is, which a native
/// call could not reproduce.
unsafe fn tail_invisible(expression: Sexp) -> bool {
    if expression.kind() != SexpType::Language || expression.car().kind() != SexpType::Symbol {
        return false;
    }
    let args = values::pairlist_items(expression.cdr());
    match &*values::symbol_name(expression.car()) {
        "<-" | "=" | "invisible" => true,
        "{" => args.last().is_some_and(|&(_, last)| tail_invisible(last)),
        "if" => args
            .iter()
            .skip(1)
            .any(|&(_, branch)| tail_invisible(branch)),
        ".ry_match" => args.iter().skip(1).any(|&(_, arm)| {
            arm.kind() == SexpType::Language
                && values::pairlist_items(arm.cdr())
                    .last()
                    .is_some_and(|&(_, body)| tail_invisible(body))
        }),
        _ => false,
    }
}

/// The type of an expression while the body is walked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Known(Ty),
    /// A recursive call's value before the function's result type is known.
    Open,
    /// An expression that never completes, such as `return(x)`.
    Never,
}

impl Kind {
    fn known(self) -> Result<Ty, Refusal> {
        match self {
            Kind::Known(ty) => Ok(ty),
            _ => Err(Refusal("the body's result type is not determined")),
        }
    }

    /// The type of a value that is either of two.
    fn join(self, other: Kind) -> Result<Kind, Refusal> {
        match (self, other) {
            (Kind::Never, kind) | (kind, Kind::Never) => Ok(kind),
            (Kind::Open, kind) | (kind, Kind::Open) => Ok(kind),
            (Kind::Known(a), Kind::Known(b)) if a == b => Ok(self),
            // The variants of a tagged union are records of different layouts.
            (Kind::Known(a), Kind::Known(b)) if a.is_record() && b.is_record() => {
                Ok(Kind::Known(Ty::Obj))
            }
            _ => Err(Refusal("the branches have different types")),
        }
    }

    /// The representation of an arithmetic operand: logicals compute as integers. `None` for
    /// an open operand.
    fn numeric(self) -> Result<Option<Ty>, Refusal> {
        match self {
            Kind::Known(Ty::Int | Ty::Lgl) => Ok(Some(Ty::Int)),
            Kind::Known(Ty::Dbl) => Ok(Some(Ty::Dbl)),
            Kind::Open => Ok(None),
            _ => Err(Refusal("an operand that is not a number")),
        }
    }
}

/// An operation of a base function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Operation {
    Add,
    Subtract,
    Multiply,
    Divide,
    Power,
    Modulo,
    IntegerDivide,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Equal,
    NotEqual,
    Not,
    And,
    Or,
    If,
    Block,
    Paren,
    Assign,
    Return,
    Abs,
    Sqrt,
    Exp,
    Log,
    Floor,
    Ceiling,
    Max,
    Min,
    For,
    While,
    Repeat,
    Break,
    Next,
    Length,
    SeqLen,
    SeqAlong,
    Colon,
    /// `x[[i]]`.
    Element,
    /// `x[i]`, which for a scalar index in range reads the same element as `x[[i]]`.
    Subset,
}

impl Operation {
    fn of(name: &str) -> Option<Operation> {
        Some(match name {
            "+" => Operation::Add,
            "-" => Operation::Subtract,
            "*" => Operation::Multiply,
            "/" => Operation::Divide,
            "^" => Operation::Power,
            "%%" => Operation::Modulo,
            "%/%" => Operation::IntegerDivide,
            "<" => Operation::Less,
            ">" => Operation::Greater,
            "<=" => Operation::LessEqual,
            ">=" => Operation::GreaterEqual,
            "==" => Operation::Equal,
            "!=" => Operation::NotEqual,
            "!" => Operation::Not,
            "&&" => Operation::And,
            "||" => Operation::Or,
            "if" => Operation::If,
            "{" => Operation::Block,
            "(" => Operation::Paren,
            "<-" | "=" => Operation::Assign,
            "return" => Operation::Return,
            "abs" => Operation::Abs,
            "sqrt" => Operation::Sqrt,
            "exp" => Operation::Exp,
            "log" => Operation::Log,
            "floor" => Operation::Floor,
            "ceiling" => Operation::Ceiling,
            "max" => Operation::Max,
            "min" => Operation::Min,
            "for" => Operation::For,
            "while" => Operation::While,
            "repeat" => Operation::Repeat,
            "break" => Operation::Break,
            "next" => Operation::Next,
            "length" => Operation::Length,
            "seq_len" => Operation::SeqLen,
            "seq_along" => Operation::SeqAlong,
            ":" => Operation::Colon,
            "[[" => Operation::Element,
            "[" => Operation::Subset,
            _ => return None,
        })
    }

    fn is_comparison(self) -> bool {
        matches!(
            self,
            Operation::Less
                | Operation::Greater
                | Operation::LessEqual
                | Operation::GreaterEqual
                | Operation::Equal
                | Operation::NotEqual
        )
    }

    fn is_binary_arithmetic(self) -> bool {
        matches!(
            self,
            Operation::Add
                | Operation::Subtract
                | Operation::Multiply
                | Operation::Divide
                | Operation::Power
                | Operation::Modulo
                | Operation::IntegerDivide
        )
    }
}

/// A ry record operation: a construction, a `match`, or a field read with `$`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RecordForm {
    Record,
    Tuple,
    Tag,
    Match,
    Dollar,
}

impl RecordForm {
    unsafe fn of(head: Sexp) -> Option<RecordForm> {
        Some(match head.car().bytes() {
            b".ry_record" => RecordForm::Record,
            b".ry_tuple" => RecordForm::Tuple,
            b".ry_tag" => RecordForm::Tag,
            b".ry_match" => RecordForm::Match,
            b"$" => RecordForm::Dollar,
            _ => return None,
        })
    }
}

/// A `match` arm's pattern, in the forms native code tests.
enum Pattern {
    /// `_`.
    Any,
    /// A name, which binds the whole value.
    Bind(Sexp),
    /// A record, tuple or tag of exactly this layout, with the name each slot binds.
    Shape {
        layout: u32,
        fields: SmallVec<[(usize, Sexp); 4]>,
    },
}

/// The pattern `pattern` is, or a refusal for one native code does not test.
unsafe fn pattern_of(pattern: Sexp) -> Result<Pattern, Refusal> {
    let refused = Refusal("a `match` pattern native code does not test");
    match pattern.kind() {
        SexpType::Symbol => match pattern.car().bytes() {
            b"_" => Ok(Pattern::Any),
            b".." | b"T" | b"F" | b"" | b"..." => Err(refused),
            _ => Ok(Pattern::Bind(pattern)),
        },
        SexpType::Language if pattern.car().kind() == SexpType::Symbol => {
            let head = pattern.car().car().bytes();
            let items = values::pairlist_items(pattern.cdr());
            if head == b"(" {
                return match items.as_slice() {
                    [(_, inner)] => pattern_of(*inner),
                    _ => Err(refused),
                };
            }
            let (tag, payload) = match head {
                b".ry_tag" => {
                    let ((name_tag, name), payload) = items.split_first().ok_or(refused)?;
                    if *name_tag != values::nil() {
                        return Err(refused);
                    }
                    (Some(*name), payload)
                }
                b".ry_record" | b".ry_tuple" => (None, &items[..]),
                _ => return Err(refused),
            };
            let names: SmallVec<[Sexp; 4]> = payload.iter().map(|&(tag, _)| tag).collect();
            let (layout, order) =
                records::constructor_layout(tag, &names, head == b".ry_record").ok_or(refused)?;
            let mut fields = SmallVec::new();
            for (&(_, item), slot) in payload.iter().zip(order) {
                match pattern_of(item)? {
                    Pattern::Any => {}
                    Pattern::Bind(name) => fields.push((slot, name)),
                    Pattern::Shape { .. } => {
                        return Err(Refusal("a nested `match` pattern"));
                    }
                }
            }
            Ok(Pattern::Shape { layout, fields })
        }
        _ => Err(refused),
    }
}

/// A shared, immutable value of a layout without slots, such as the tag `:North`: one object
/// serves every construction, so building one allocates nothing.
unsafe fn singleton(layout: u32) -> Result<Sexp, Refusal> {
    thread_local! {
        static SINGLETONS: RefCell<FxHashMap<u32, Sexp>> = RefCell::default();
    }
    if let Some(value) = SINGLETONS.with_borrow(|singletons| singletons.get(&layout).copied()) {
        return Ok(value);
    }
    let value = records::construct(layout, &[])
        .map_err(|_| Refusal("a value without slots that cannot be allocated"))?;
    value.mark_not_mutable();
    interp::heap().add_global_root(value);
    SINGLETONS.with_borrow_mut(|singletons| singletons.insert(layout, value));
    Ok(value)
}

/// The slot of `layout` that holds the field `name`.
/// Whether a layout `$` and `[[` may read is positional (a tuple) or named (a record). A tag's
/// payload is read only by `match`, and a dict or a set has no fields, so the interpreter raises
/// an error for both and native code leaves them to it.
unsafe fn plain_fields(layout: u32) -> Result<bool, Refusal> {
    let refused = Refusal("`$` or `[[` of a tag, a dict or a set");
    let shape = interp::heap().layouts.get(layout).ok_or(refused)?;
    match shape.fields {
        _ if shape.tag.is_some() => Err(refused),
        Fields::Positional(_) => Ok(true),
        Fields::Named(_) => Ok(false),
        Fields::Dict | Fields::Set => Err(refused),
    }
}

unsafe fn slot_named(layout: u32, name: Sexp) -> Result<usize, Refusal> {
    interp::heap()
        .layouts
        .get(layout)
        .and_then(|shape| shape.names().iter().position(|&field| field == name))
        .ok_or(Refusal("a field the record does not have"))
}

/// A walk over a body. Without an emitter it types the body and checks that it is a scalar
/// function; with one it also emits the code, so both make the same decisions.
struct Walker<'p> {
    function: Sexp,
    /// Where the body being walked looks up the functions it calls: the function's own
    /// environment, or an inlined callee's.
    environment: Sexp,
    /// The callees whose bodies enclose the expression being walked, innermost last.
    inlined: SmallVec<[Sexp; 2]>,
    /// The function's own result type, once known.
    own_result: Option<Ty>,
    parameters: &'p [(Sexp, Ty)],
    /// Parameters and locals: the symbol, its type, and its variable once emitted. A local
    /// first assigned in an `if` branch, a right operand of `&&` or `||`, or a loop goes out of
    /// scope where that region ends, because it is not bound on every path past it: a later
    /// read refuses the function rather than guess what R would find.
    locals: SmallVec<[(Sexp, Ty, Option<Variable>); 8]>,
    /// For each loop enclosing the expression being walked, innermost last, whether a `break`
    /// leaves it.
    breaks: SmallVec<[bool; 4]>,
    /// How many closure call arguments enclose the expression being walked. R passes them as
    /// promises the callee may force late or never, so evaluating one eagerly is only unobservable
    /// when it neither assigns nor returns.
    lazy: usize,
    /// Whether the expression about to be walked is a statement whose value nobody reads.
    statement: bool,
    nesting: usize,
    /// The join of the types `return` passes back.
    returned: Kind,
    speculations: Vec<Speculation>,
    /// Whether the body allocates a record, itself or through a callee. The emitting walk gets
    /// it from the typing walk, and roots and releases accordingly.
    allocates: bool,
    /// For each loop enclosing the expression being walked, innermost last: how many locals
    /// were in scope where it starts, and whether its body assigns a record to one of them.
    /// Every other record an iteration allocates is dead when the next one starts, so the loop
    /// releases them there unless it carries one.
    loop_scopes: SmallVec<[(usize, bool); 4]>,
}

/// An expression's type, and its machine value when emitting and the expression completes.
#[derive(Clone, Copy)]
struct Val {
    kind: Kind,
    value: Option<Value>,
}

type Emitting<'e, 'f> = Option<&'e mut Emitter<'f>>;

impl<'p> Walker<'p> {
    fn new(function: Sexp, parameters: &'p [(Sexp, Ty)], own_result: Option<Ty>) -> Walker<'p> {
        Walker {
            function,
            // SAFETY: `function` is a closure, whose tag is its environment.
            environment: unsafe { function.tag() },
            inlined: SmallVec::new(),
            own_result,
            parameters,
            locals: parameters
                .iter()
                .map(|&(symbol, ty)| (symbol, ty, None))
                .collect(),
            breaks: SmallVec::new(),
            lazy: 0,
            statement: false,
            nesting: 0,
            returned: Kind::Never,
            speculations: Vec::new(),
            allocates: false,
            loop_scopes: SmallVec::new(),
        }
    }

    /// The type of the function's value: the body's joined with every `return`.
    unsafe fn walk(&mut self, emitter: Emitting<'_, '_>, body: Sexp) -> Result<Val, Refusal> {
        let mut emitter = emitter;
        let value = self.expression(&mut emitter, body)?;
        Ok(Val {
            kind: value.kind.join(self.returned)?,
            value: value.value,
        })
    }

    unsafe fn expression(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        expression: Sexp,
    ) -> Result<Val, Refusal> {
        self.nesting += 1;
        if self.nesting > MAX_NESTING {
            return Err(Refusal("the body nests too deeply"));
        }
        let result = self.expression_unbounded(emitter, expression);
        self.nesting -= 1;
        result
    }

    unsafe fn expression_unbounded(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        expression: Sexp,
    ) -> Result<Val, Refusal> {
        let statement = std::mem::take(&mut self.statement);
        match expression.kind() {
            SexpType::Integer | SexpType::Real | SexpType::Logical => {
                let ty = constant_type(expression);
                if !ty.is_unboxed() {
                    return Err(Refusal("a constant that is not a plain scalar"));
                }
                let value = emitter.as_mut().map(|emitter| match ty {
                    Ty::Dbl => emitter
                        .builder
                        .ins()
                        .f64const(Ieee64::with_bits(values::reals(expression)[0].to_bits())),
                    _ => emitter
                        .builder
                        .ins()
                        .iconst(I32, values::ints(expression)[0] as i64),
                });
                Ok(Val {
                    kind: Kind::Known(ty),
                    value,
                })
            }
            SexpType::Symbol => {
                let &(_, ty, variable) = self
                    .locals
                    .iter()
                    .rev()
                    .find(|(symbol, _, _)| *symbol == expression)
                    .ok_or(Refusal(
                        "a variable that is neither a parameter nor a local",
                    ))?;
                let value = match emitter.as_mut() {
                    Some(emitter) => Some(
                        emitter
                            .builder
                            .use_var(variable.ok_or(Refusal("a local without a variable"))?),
                    ),
                    None => None,
                };
                Ok(Val {
                    kind: Kind::Known(ty),
                    value,
                })
            }
            SexpType::Language => {
                let head = expression.car();
                if head.kind() != SexpType::Symbol {
                    return Err(Refusal("a call whose function is computed"));
                }
                let items = values::pairlist_items(expression.cdr());
                if let Some(form) = RecordForm::of(head)
                    && !self.locals.iter().any(|(symbol, _, _)| *symbol == head)
                {
                    let speculation = resolve_function(head, self.environment)
                        .ok_or(Refusal("a function the lookup cannot resolve"))?;
                    if base_builtin(speculation.function, head).is_none() {
                        return Err(Refusal("a record operation rebound to another function"));
                    }
                    self.speculations.push(speculation);
                    return self.record_form(emitter, form, &items);
                }
                if items.iter().any(|&(tag, value)| {
                    tag != values::nil()
                        || value == values::missing_arg()
                        || value == values::symbols().dots
                }) {
                    return Err(Refusal("a call with named, missing or `...` arguments"));
                }
                let args: SmallVec<[Sexp; 4]> = items.iter().map(|&(_, value)| value).collect();
                if self.locals.iter().any(|(symbol, _, _)| *symbol == head) {
                    return Err(Refusal("a call of a local variable"));
                }
                let speculation = resolve_function(head, self.environment)
                    .ok_or(Refusal("a function the lookup cannot resolve"))?;
                let callee = speculation.function;
                self.speculations.push(speculation);
                if callee.kind() == SexpType::Closure {
                    return self.call(emitter, callee, &args);
                }
                let name = base_builtin(callee, head).ok_or(Refusal(
                    "a function that is neither a closure nor a base function",
                ))?;
                let operation =
                    Operation::of(&name).ok_or(Refusal("a base function native code lacks"))?;
                self.builtin(emitter, operation, &args, statement)
            }
            _ => Err(Refusal("an expression that is not a scalar computation")),
        }
    }

    /// A call of a closure: of this function itself with the same types, or of another native
    /// function.
    unsafe fn call(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        callee: Sexp,
        args: &[Sexp],
    ) -> Result<Val, Refusal> {
        let mut kinds: SmallVec<[Kind; 4]> = SmallVec::new();
        let mut arguments: SmallVec<[Value; 4]> = SmallVec::new();
        self.lazy += 1;
        for &arg in args {
            let value = self.expression(emitter, arg);
            let value = match value {
                Ok(value) => value,
                Err(refusal) => {
                    self.lazy -= 1;
                    return Err(refusal);
                }
            };
            if value.kind == Kind::Never {
                self.lazy -= 1;
                return Ok(value);
            }
            kinds.push(value.kind);
            arguments.extend(value.value);
        }
        self.lazy -= 1;
        let Some(types) = kinds
            .iter()
            .map(|kind| match kind {
                Kind::Known(ty) => Some(*ty),
                _ => None,
            })
            .collect::<Option<SmallVec<[Ty; 4]>>>()
        else {
            // An argument's type waits for the function's own result type.
            return Ok(Val {
                kind: Kind::Open,
                value: None,
            });
        };
        let own = callee == self.function
            && types.len() == self.parameters.len()
            && types
                .iter()
                .zip(self.parameters)
                .all(|(&ty, &(_, own))| ty == own);
        if own {
            // Before the result type is known, the `return`s walked so far suggest it.
            let kind = match (self.own_result, self.returned) {
                (Some(ty), _) => Kind::Known(ty),
                (None, Kind::Known(ty)) => Kind::Known(ty),
                (None, _) => Kind::Open,
            };
            let value = match emitter.as_mut() {
                Some(emitter) => {
                    let reference = emitter.own;
                    Some(emitter.call(Callee::Own(reference), &arguments)?)
                }
                None => None,
            };
            return Ok(Val { kind, value });
        }
        let native = native_for(callee, &types)
            .ok_or(Refusal("a call of a closure that is not a native function"))?;
        self.speculations
            .extend(native.speculations.iter().cloned());
        self.allocates |= native.allocates;
        if self.inlined.len() < MAX_INLINE_DEPTH && !self.inlined.contains(&callee) {
            let body = bytecode::closure_body(callee);
            if inlinable(body) {
                return self.inline(emitter, callee, native, body, &types, &arguments);
            }
        }
        let value = emitter
            .as_mut()
            .map(|emitter| emitter.call(Callee::Other(native), &arguments))
            .transpose()?;
        Ok(Val {
            kind: Kind::Known(native.result),
            value,
        })
    }

    /// A call of a small native function compiled into the caller: its body walked in its own
    /// scope with its parameters bound to the argument values. The call is invisible in either
    /// form, so this only saves the call and lets the caller's code see the callee's.
    unsafe fn inline(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        callee: Sexp,
        native: &Native,
        body: Sexp,
        types: &[Ty],
        arguments: &[Value],
    ) -> Result<Val, Refusal> {
        let mut locals: SmallVec<[(Sexp, Ty, Option<Variable>); 8]> = SmallVec::new();
        for (index, (&(symbol, _), &ty)) in values::pairlist_items(callee.car())
            .iter()
            .zip(types)
            .enumerate()
        {
            let variable = match emitter.as_mut() {
                Some(emitter) => {
                    let argument = *arguments
                        .get(index)
                        .ok_or(Refusal("an inlined call without its argument"))?;
                    let variable = emitter.builder.declare_var(machine_type(ty));
                    emitter.builder.def_var(variable, argument);
                    Some(variable)
                }
                None => None,
            };
            locals.push((symbol, ty, variable));
        }
        let caller_locals = std::mem::replace(&mut self.locals, locals);
        let caller_breaks = std::mem::take(&mut self.breaks);
        let caller_loops = std::mem::take(&mut self.loop_scopes);
        let caller_environment = std::mem::replace(&mut self.environment, callee.tag());
        let caller_nesting = std::mem::take(&mut self.nesting);
        self.inlined.push(callee);
        let value = self.expression(emitter, body);
        self.inlined.pop();
        self.nesting = caller_nesting;
        self.environment = caller_environment;
        self.breaks = caller_breaks;
        self.loop_scopes = caller_loops;
        self.locals = caller_locals;
        let value = value?;
        match value.kind {
            Kind::Known(ty) if ty == native.result => Ok(value),
            Kind::Never => Ok(value),
            _ => Err(Refusal(
                "an inlined body of a different type than its function",
            )),
        }
    }

    unsafe fn builtin(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        operation: Operation,
        args: &[Sexp],
        statement: bool,
    ) -> Result<Val, Refusal> {
        match (operation, args) {
            (Operation::Paren, [inner]) => self.expression(emitter, *inner),
            (Operation::Block, [_, ..]) => self.block(emitter, args),
            (Operation::Assign, [target, value]) => self.assign(emitter, *target, *value),
            (Operation::Return, _) if self.lazy > 0 => Err(Refusal("a `return` inside a promise")),
            (Operation::Return, [value]) => {
                let value = self.expression(emitter, *value)?;
                self.returned = self.returned.join(value.kind)?;
                if let Some(emitter) = emitter.as_mut()
                    && let Some(value) = value.value
                {
                    emitter.finish(value);
                }
                Ok(Val {
                    kind: Kind::Never,
                    value: None,
                })
            }
            (Operation::If, [condition, yes, no]) => {
                self.branch(emitter, *condition, *yes, Some(*no))
            }
            (Operation::If, [condition, yes]) if statement => {
                self.branch(emitter, *condition, *yes, None)
            }
            (Operation::If, [_, _]) => Err(Refusal("an `if` without `else` used for its value")),
            (Operation::For | Operation::While | Operation::Repeat, _) if !statement => {
                Err(Refusal("a loop used for its value"))
            }
            (Operation::For | Operation::While | Operation::Repeat, _) if self.lazy > 0 => {
                Err(Refusal("a loop inside a promise"))
            }
            (Operation::For, [variable, sequence, body]) => {
                self.for_loop(emitter, *variable, *sequence, *body)
            }
            (Operation::While, [condition, body]) => {
                self.loop_body(emitter, Some(*condition), *body)
            }
            (Operation::Repeat, [body]) => self.loop_body(emitter, None, *body),
            (Operation::Break | Operation::Next, []) => {
                if self.lazy > 0 {
                    return Err(Refusal("a `break` or `next` inside a promise"));
                }
                let breaks = self
                    .breaks
                    .last_mut()
                    .ok_or(Refusal("a `break` or `next` outside a loop"))?;
                *breaks |= operation == Operation::Break;
                if let Some(emitter) = emitter.as_mut() {
                    let &(next, exit) = emitter
                        .loops
                        .last()
                        .ok_or(Refusal("a `break` or `next` outside a loop"))?;
                    let target = if operation == Operation::Break {
                        exit
                    } else {
                        next
                    };
                    emitter.builder.ins().jump(target, &[]);
                    let dead = emitter.builder.create_block();
                    emitter.builder.switch_to_block(dead);
                }
                Ok(Val {
                    kind: Kind::Never,
                    value: None,
                })
            }
            (Operation::Length, [operand]) => {
                let operand = self.expression(emitter, *operand)?;
                let value = match (operand.kind, emitter.as_mut(), operand.value) {
                    (Kind::Never, ..) => return Ok(operand),
                    (Kind::Known(ty), Some(emitter), Some(vector)) if ty.is_vector() => {
                        Some(emitter.length(vector))
                    }
                    (Kind::Known(ty), emitter, _) if ty.is_unboxed() => {
                        emitter.map(|emitter| emitter.builder.ins().iconst(I32, 1))
                    }
                    (Kind::Known(ty), None, _) if ty.is_vector() => None,
                    (Kind::Open, None, _) => None,
                    _ => return Err(Refusal("`length` of something native code lacks")),
                };
                Ok(Val {
                    kind: Kind::Known(Ty::Int),
                    value,
                })
            }
            (Operation::Element | Operation::Subset, [vector, index]) => {
                let vector = self.expression(emitter, *vector)?;
                if vector.kind == Kind::Never {
                    return Ok(vector);
                }
                let Kind::Known(vector_ty) = vector.kind else {
                    return Err(Refusal("an index into something other than a vector"));
                };
                if vector_ty.is_record() && operation == Operation::Element {
                    return self.record_item(emitter, vector, *index);
                }
                if !vector_ty.is_vector() {
                    return Err(Refusal("an index into something other than a vector"));
                }
                let index = self.expression(emitter, *index)?;
                if index.kind == Kind::Never {
                    return Ok(index);
                }
                // `x[TRUE]` recycles the logical over the whole vector, while `x[[TRUE]]` reads the
                // first element, so only `[[` may treat a logical index as a position.
                if operation == Operation::Subset && index.kind == Kind::Known(Ty::Lgl) {
                    return Err(Refusal("`[` with a logical index selects, not indexes"));
                }
                let index_ty = index.kind.numeric()?;
                let value = match (emitter.as_mut(), vector.value, index.value, index_ty) {
                    (Some(emitter), Some(vector), Some(index), Some(index_ty)) => {
                        Some(emitter.element(vector_ty.element(), vector, index_ty, index))
                    }
                    (Some(_), ..) => return Err(Refusal("an index without a value")),
                    (None, ..) => None,
                };
                Ok(Val {
                    kind: Kind::Known(vector_ty.element()),
                    value,
                })
            }
            (Operation::And | Operation::Or, [left, right]) => {
                self.short_circuit(emitter, operation == Operation::And, *left, *right)
            }
            (Operation::Subtract | Operation::Add, [operand]) => {
                let operand = self.expression(emitter, *operand)?;
                let Some(ty) = operand.kind.numeric()? else {
                    return Ok(operand);
                };
                let value = match (emitter.as_mut(), operand.value) {
                    (Some(emitter), Some(value)) if operation == Operation::Subtract => {
                        Some(emitter.negate(ty, value))
                    }
                    _ => operand.value,
                };
                Ok(Val {
                    kind: Kind::Known(ty),
                    value,
                })
            }
            (Operation::Not, [operand]) => {
                let operand = self.expression(emitter, *operand)?;
                if !matches!(operand.kind, Kind::Known(Ty::Lgl | Ty::Int) | Kind::Open) {
                    return Err(Refusal("`!` of a double"));
                }
                let value = match (emitter.as_mut(), operand.value) {
                    (Some(emitter), Some(value)) => Some(emitter.not(value)),
                    _ => None,
                };
                Ok(Val {
                    kind: Kind::Known(Ty::Lgl),
                    value,
                })
            }
            (
                Operation::Abs
                | Operation::Sqrt
                | Operation::Exp
                | Operation::Log
                | Operation::Floor
                | Operation::Ceiling,
                [operand],
            ) => {
                let operand = self.expression(emitter, *operand)?;
                let Some(ty) = operand.kind.numeric()? else {
                    let kind = if operation == Operation::Abs {
                        Kind::Open
                    } else {
                        Kind::Known(Ty::Dbl)
                    };
                    return Ok(Val { kind, value: None });
                };
                let result = if operation == Operation::Abs && ty == Ty::Int {
                    Ty::Int
                } else {
                    Ty::Dbl
                };
                let value = match (emitter.as_mut(), operand.value) {
                    (Some(emitter), Some(value)) => Some(emitter.math(operation, ty, value)),
                    _ => None,
                };
                Ok(Val {
                    kind: Kind::Known(result),
                    value,
                })
            }
            (Operation::Max | Operation::Min, [left, right]) => {
                let left = self.expression(emitter, *left)?;
                if left.kind == Kind::Never {
                    return Ok(left);
                }
                let right = self.expression(emitter, *right)?;
                if right.kind == Kind::Never {
                    return Ok(right);
                }
                let (Some(a), Some(b)) = (left.kind.numeric()?, right.kind.numeric()?) else {
                    return Ok(Val {
                        kind: Kind::Open,
                        value: None,
                    });
                };
                let integers = a == Ty::Int && b == Ty::Int;
                let value = match (emitter.as_mut(), left.value, right.value) {
                    (Some(emitter), Some(x), Some(y)) => {
                        Some(emitter.extreme(operation == Operation::Max, integers, (a, x), (b, y)))
                    }
                    _ => None,
                };
                Ok(Val {
                    kind: Kind::Known(if integers { Ty::Int } else { Ty::Dbl }),
                    value,
                })
            }
            (_, [left, right]) if operation.is_comparison() || operation.is_binary_arithmetic() => {
                let left = self.expression(emitter, *left)?;
                if left.kind == Kind::Never {
                    return Ok(left);
                }
                let right = self.expression(emitter, *right)?;
                if right.kind == Kind::Never {
                    return Ok(right);
                }
                let (Some(a), Some(b)) = (left.kind.numeric()?, right.kind.numeric()?) else {
                    // Only the second walk, with the function's result type known, decides.
                    let kind = if operation.is_comparison() {
                        Kind::Known(Ty::Lgl)
                    } else {
                        Kind::Open
                    };
                    return Ok(Val { kind, value: None });
                };
                let integers = a == Ty::Int && b == Ty::Int;
                let result = match operation {
                    _ if operation.is_comparison() => Ty::Lgl,
                    Operation::Divide | Operation::Power => Ty::Dbl,
                    Operation::IntegerDivide if !integers => {
                        return Err(Refusal("`%/%` of doubles"));
                    }
                    _ if integers => Ty::Int,
                    _ => Ty::Dbl,
                };
                let value = match (emitter.as_mut(), left.value, right.value) {
                    (Some(emitter), Some(x), Some(y)) => {
                        Some(emitter.binary(operation, integers, (a, x), (b, y)))
                    }
                    _ => None,
                };
                Ok(Val {
                    kind: Kind::Known(result),
                    value,
                })
            }
            _ => Err(Refusal("a base function call native code lacks")),
        }
    }

    /// A record construction, a `match`, or `record$field`.
    unsafe fn record_form(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        form: RecordForm,
        items: &[(Sexp, Sexp)],
    ) -> Result<Val, Refusal> {
        if items
            .iter()
            .any(|&(_, value)| value == values::missing_arg() || value == values::symbols().dots)
        {
            return Err(Refusal(
                "a record operation with missing or `...` arguments",
            ));
        }
        match form {
            RecordForm::Match => self.match_arms(emitter, items),
            RecordForm::Dollar => {
                let &[(_, subject), (_, field)] = items else {
                    return Err(Refusal("a `$` without a record and a field"));
                };
                let subject = self.expression(emitter, subject)?;
                let layout = match subject.kind {
                    Kind::Never => return Ok(subject),
                    Kind::Known(Ty::Rec(layout)) => layout,
                    _ => {
                        return Err(Refusal(
                            "`$` of something other than a record of known shape",
                        ));
                    }
                };
                let name = match field.kind() {
                    SexpType::Symbol => field,
                    SexpType::String if values::length(field) == 1 => {
                        values::install_charsxp(values::string_elt(field, 0))
                            .map_err(|_| Refusal("a field name that cannot be interned"))?
                    }
                    _ => return Err(Refusal("`$` with a computed field")),
                };
                plain_fields(layout)?;
                let index = slot_named(layout, name)?;
                self.read_field(emitter, subject.value, layout, index)
            }
            RecordForm::Record | RecordForm::Tuple | RecordForm::Tag => {
                self.construct(emitter, form, items)
            }
        }
    }

    /// A record, tuple or tag built from its items, evaluated in source order.
    unsafe fn construct(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        form: RecordForm,
        items: &[(Sexp, Sexp)],
    ) -> Result<Val, Refusal> {
        let (tag, payload) = if form == RecordForm::Tag {
            let ((name_tag, name), payload) =
                items.split_first().ok_or(Refusal("a tag without a name"))?;
            if *name_tag != values::nil()
                || name.kind() != SexpType::String
                || values::length(*name) != 1
            {
                return Err(Refusal("a tag whose name is computed"));
            }
            (Some(*name), payload)
        } else {
            (None, items)
        };
        let names: SmallVec<[Sexp; 4]> = payload.iter().map(|&(tag, _)| tag).collect();
        let (layout, order) = records::constructor_layout(tag, &names, form == RecordForm::Record)
            .ok_or(Refusal("a construction whose shape R rejects"))?;
        let mut slots: SmallVec<[(Ty, Value); 4]> = SmallVec::new();
        let mut placed: SmallVec<[usize; 4]> = SmallVec::new();
        for (&(_, item), &slot) in payload.iter().zip(&order) {
            let value = self.expression(emitter, item)?;
            match value.kind {
                Kind::Never => return Ok(value),
                Kind::Known(ty) if ty.is_unboxed() || ty.is_record() => {
                    interp::heap().layouts.observe(layout, slot, kind_bit(ty));
                    if let Some(machine) = value.value {
                        slots.push((ty, machine));
                        placed.push(slot);
                    }
                }
                Kind::Known(_) => {
                    return Err(Refusal("a field that is neither a scalar nor a record"));
                }
                Kind::Open => {}
            }
        }
        let kind = Kind::Known(Ty::Rec(layout));
        if order.is_empty() {
            let value = singleton(layout)?;
            let value = emitter
                .as_mut()
                .map(|emitter| emitter.builder.ins().iconst(I64, value.as_ptr() as i64));
            return Ok(Val { kind, value });
        }
        self.allocates = true;
        let Some(emitter) = emitter.as_mut() else {
            return Ok(Val { kind, value: None });
        };
        if slots.len() != order.len() {
            return Err(Refusal("a field without a value"));
        }
        let size = u32::try_from(16 * order.len())
            .map_err(|_| Refusal("a record with too many fields"))?;
        let pairs = emitter.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            size,
            3,
        ));
        for (&(ty, machine), &slot) in slots.iter().zip(&placed) {
            let (representation, bits) = match ty {
                Ty::Dbl => (
                    runtime::representation::DOUBLE,
                    emitter
                        .builder
                        .ins()
                        .bitcast(I64, MemFlagsData::new(), machine),
                ),
                Ty::Int => (
                    runtime::representation::INTEGER,
                    emitter.builder.ins().uextend(I64, machine),
                ),
                Ty::Lgl => (
                    runtime::representation::LOGICAL,
                    emitter.builder.ins().uextend(I64, machine),
                ),
                _ => (runtime::representation::BOXED, machine),
            };
            let offset = 16 * slot as i32;
            let representation = emitter.builder.ins().iconst(I64, i64::from(representation));
            emitter
                .builder
                .ins()
                .stack_store(I64, representation, pairs, offset);
            emitter
                .builder
                .ins()
                .stack_store(I64, bits, pairs, offset + 8);
        }
        let address = emitter.builder.ins().stack_addr(I64, pairs, 0);
        let layout_id = emitter.builder.ins().iconst(I32, i64::from(layout));
        let count = emitter.builder.ins().iconst(I64, order.len() as i64);
        let record = emitter.helper(
            runtime::jit_native_record as *const () as usize,
            &[I32, I64, I64],
            I64,
            &[layout_id, address, count],
        );
        let failed = emitter.builder.ins().icmp_imm_s(IntCC::Equal, record, 0);
        emitter.fail_if(failed);
        Ok(Val {
            kind,
            value: Some(record),
        })
    }

    /// `record[[index]]` with a constant index: a position into a tuple or a name into a record.
    unsafe fn record_item(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        record: Val,
        index: Sexp,
    ) -> Result<Val, Refusal> {
        let Kind::Known(Ty::Rec(layout)) = record.kind else {
            return Err(Refusal("`[[` of a record of unknown shape"));
        };
        let refused = Refusal("`[[` of a record with an index native code does not resolve");
        let positional = plain_fields(layout)?;
        let slot = match index.kind() {
            SexpType::String if values::length(index) == 1 && !positional => slot_named(
                layout,
                values::install_charsxp(values::string_elt(index, 0))
                    .map_err(|_| Refusal("a field name that cannot be interned"))?,
            )?,
            SexpType::Integer | SexpType::Real if values::length(index) == 1 && positional => {
                let position = if index.kind() == SexpType::Integer {
                    f64::from(values::ints(index)[0])
                } else {
                    values::reals(index)[0]
                };
                let count = interp::heap()
                    .layouts
                    .get(layout)
                    .map_or(0, |shape| shape.slot_count());
                if !(position >= 1.0 && position < count as f64 + 1.0)
                    || index.kind() == SexpType::Integer && values::ints(index)[0] == NA_INTEGER
                {
                    return Err(refused);
                }
                position as usize - 1
            }
            _ => return Err(refused),
        };
        self.read_field(emitter, record.value, layout, slot)
    }

    /// Slot `index` of a record of `layout`, typed from the slot's profile and checked.
    unsafe fn read_field(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        record: Option<Value>,
        layout: u32,
        index: usize,
    ) -> Result<Val, Refusal> {
        let Some(ty) = field_type(layout, index)? else {
            // No value of the layout has filled the slot, so nothing predicts its type.
            if let Some(emitter) = emitter.as_mut() {
                emitter.builder.ins().jump(emitter.failure, &[]);
                let dead = emitter.builder.create_block();
                emitter.builder.switch_to_block(dead);
            }
            return Ok(Val {
                kind: Kind::Never,
                value: None,
            });
        };
        let value = match (emitter.as_mut(), record) {
            (Some(emitter), Some(record)) => Some(emitter.field(ty, record, index)?),
            (Some(_), None) => return Err(Refusal("a record without a value")),
            (None, _) => None,
        };
        Ok(Val {
            kind: Kind::Known(ty),
            value,
        })
    }

    /// `match (subject) { pattern => body ... }` over a record, tuple or tag.
    unsafe fn match_arms(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        items: &[(Sexp, Sexp)],
    ) -> Result<Val, Refusal> {
        if self.lazy > 0 {
            return Err(Refusal("a `match` inside a promise"));
        }
        let ((_, subject), arms) = items
            .split_first()
            .ok_or(Refusal("a `match` without a subject"))?;
        let mut parsed: SmallVec<[(Pattern, Sexp); 4]> = SmallVec::new();
        for &(tag, arm) in arms {
            let parts = if arm.kind() == SexpType::Language
                && arm.car().kind() == SexpType::Symbol
                && arm.car().car().bytes() == b"=>"
            {
                values::pairlist_items(arm.cdr())
            } else {
                Vec::new()
            };
            let [(_, pattern), (_, body)] = parts.as_slice() else {
                return Err(Refusal("a `match` arm with a guard"));
            };
            if tag != values::nil() {
                return Err(Refusal("a named `match` arm"));
            }
            parsed.push((pattern_of(*pattern)?, *body));
        }
        let subject = self.expression(emitter, *subject)?;
        let subject_ty = match subject.kind {
            Kind::Never => return Ok(subject),
            Kind::Known(ty) if ty.is_record() => ty,
            _ => return Err(Refusal("a `match` on something other than a record")),
        };
        let id = match (emitter.as_mut(), subject.value, subject_ty) {
            (Some(emitter), Some(record), Ty::Obj) => Some(emitter.builder.ins().load(
                I32,
                MemFlagsData::trusted(),
                record,
                RECORD_LAYOUT_OFFSET as i32,
            )),
            _ => None,
        };
        let join = emitter
            .as_mut()
            .map(|emitter| emitter.builder.create_block());
        let mut kind = Kind::Never;
        let mut exhaustive = false;
        for (pattern, body) in &parsed {
            let layout = match pattern {
                Pattern::Shape { layout, .. } => Some(*layout),
                _ => None,
            };
            if let (Ty::Rec(own), Some(layout)) = (subject_ty, layout)
                && own != layout
            {
                continue;
            }
            let otherwise = match (emitter.as_mut(), id, layout) {
                (Some(emitter), Some(id), Some(layout)) => {
                    let same =
                        emitter
                            .builder
                            .ins()
                            .icmp_imm_s(IntCC::Equal, id, i64::from(layout));
                    let arm = emitter.builder.create_block();
                    let otherwise = emitter.builder.create_block();
                    emitter.builder.ins().brif(same, arm, &[], otherwise, &[]);
                    emitter.builder.switch_to_block(arm);
                    Some(otherwise)
                }
                _ => None,
            };
            let scope = self.locals.len();
            let value = self.arm(emitter, pattern, subject, body)?;
            self.locals.truncate(scope);
            kind = kind.join(value.kind)?;
            if let (Some(emitter), Some(join)) = (emitter.as_mut(), join) {
                emitter.close_branch(join, value, true)?;
                if let Some(otherwise) = otherwise {
                    emitter.builder.switch_to_block(otherwise);
                }
            }
            if layout.is_none() || matches!(subject_ty, Ty::Rec(_)) {
                exhaustive = true;
                break;
            }
        }
        let Some(emitter) = emitter.as_mut() else {
            return Ok(Val { kind, value: None });
        };
        let join = join.ok_or(Refusal("a `match` without a join"))?;
        if !exhaustive {
            // R raises an error when no arm matches.
            emitter.builder.ins().jump(emitter.failure, &[]);
        }
        emitter.builder.switch_to_block(join);
        let value = emitter.builder.block_params(join).first().copied();
        Ok(Val { kind, value })
    }

    /// One `match` arm whose pattern holds: its bindings, then its body.
    unsafe fn arm(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        pattern: &Pattern,
        subject: Val,
        body: &Sexp,
    ) -> Result<Val, Refusal> {
        let subject_ty = subject.kind.known()?;
        match pattern {
            Pattern::Any => {}
            Pattern::Bind(name) => self.bind(emitter, *name, subject_ty, subject.value)?,
            Pattern::Shape { layout, fields } => {
                for &(slot, name) in fields {
                    let field = self.read_field(emitter, subject.value, *layout, slot)?;
                    let Kind::Known(ty) = field.kind else {
                        return Ok(field);
                    };
                    self.bind(emitter, name, ty, field.value)?;
                }
            }
        }
        self.expression(emitter, *body)
    }

    unsafe fn block(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        statements: &[Sexp],
    ) -> Result<Val, Refusal> {
        let mut last = Val {
            kind: Kind::Never,
            value: None,
        };
        for (index, &statement) in statements.iter().enumerate() {
            self.statement = index + 1 < statements.len();
            last = self.expression(emitter, statement)?;
            if last.kind == Kind::Never {
                // Nothing after a `return` runs.
                break;
            }
        }
        Ok(last)
    }

    unsafe fn assign(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        target: Sexp,
        value: Sexp,
    ) -> Result<Val, Refusal> {
        if target.kind() != SexpType::Symbol {
            return Err(Refusal("an assignment to something other than a variable"));
        }
        if self.lazy > 0 {
            return Err(Refusal("an assignment inside a promise"));
        }
        let value = self.expression(emitter, value)?;
        let ty = match value.kind {
            Kind::Never => return Ok(value),
            Kind::Known(ty) => ty,
            Kind::Open => return Err(Refusal("an assignment of an undetermined value")),
        };
        self.bind(emitter, target, ty, value.value)?;
        Ok(value)
    }

    /// Binds the local `target` to a value of type `ty`, as an assignment or a `match` pattern
    /// does.
    unsafe fn bind(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        target: Sexp,
        ty: Ty,
        machine: Option<Value>,
    ) -> Result<(), Refusal> {
        let existing = self
            .locals
            .iter()
            .rposition(|(symbol, _, _)| *symbol == target);
        let variable = match existing {
            Some(index) => {
                if self.locals[index].1 != ty {
                    return Err(Refusal("a variable assigned values of different types"));
                }
                if ty.is_record() {
                    for (scope, carries) in &mut self.loop_scopes {
                        *carries |= index < *scope;
                    }
                }
                self.locals[index].2
            }
            None => {
                let variable = emitter
                    .as_mut()
                    .map(|emitter| emitter.builder.declare_var(machine_type(ty)));
                self.locals.push((target, ty, variable));
                variable
            }
        };
        if let (Some(emitter), Some(variable), Some(machine)) =
            (emitter.as_mut(), variable, machine)
        {
            emitter.builder.def_var(variable, machine);
        }
        Ok(())
    }

    /// `if (condition) yes else no`, or without `no` as a statement whose value nobody reads.
    unsafe fn branch(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        condition: Sexp,
        yes: Sexp,
        no: Option<Sexp>,
    ) -> Result<Val, Refusal> {
        let condition = self.expression(emitter, condition)?;
        if condition.kind == Kind::Never {
            return Ok(condition);
        }
        let condition_ty = condition.kind.numeric()?;
        let labels = match (emitter.as_mut(), condition.value) {
            (Some(emitter), Some(value)) => {
                let ty = condition_ty.ok_or(Refusal("a condition of undetermined type"))?;
                let truth = emitter.truth(ty, value);
                let yes_label = emitter.builder.create_block();
                let no_label = emitter.builder.create_block();
                let join = emitter.builder.create_block();
                emitter
                    .builder
                    .ins()
                    .brif(truth, yes_label, &[], no_label, &[]);
                emitter.builder.switch_to_block(yes_label);
                Some((no_label, join))
            }
            (Some(_), None) => return Err(Refusal("a condition without a value")),
            (None, _) => None,
        };
        let used = no.is_some();
        let scope = self.locals.len();
        let yes = self.expression(emitter, yes)?;
        self.locals.truncate(scope);
        if let (Some(emitter), Some((no_label, join))) = (emitter.as_mut(), labels) {
            emitter.close_branch(join, yes, used)?;
            emitter.builder.switch_to_block(no_label);
        }
        let no = match no {
            Some(no) => self.expression(emitter, no)?,
            None => Val {
                kind: Kind::Known(Ty::Bottom),
                value: None,
            },
        };
        self.locals.truncate(scope);
        let kind = if used {
            yes.kind.join(no.kind)?
        } else {
            Kind::Known(Ty::Bottom)
        };
        let value = match (emitter.as_mut(), labels) {
            (Some(emitter), Some((_, join))) => {
                emitter.close_branch(join, no, used)?;
                emitter.builder.switch_to_block(join);
                emitter.builder.block_params(join).first().copied()
            }
            _ => None,
        };
        Ok(Val { kind, value })
    }

    /// `for (variable in sequence) body`, as a statement.
    unsafe fn for_loop(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        variable: Sexp,
        sequence: Sexp,
        body: Sexp,
    ) -> Result<Val, Refusal> {
        if variable.kind() != SexpType::Symbol {
            return Err(Refusal("a loop variable that is not a symbol"));
        }
        let Some(sequence) = self.sequence(emitter, sequence)? else {
            return Ok(Val {
                kind: Kind::Never,
                value: None,
            });
        };
        let scope = self.locals.len();
        let existing = self
            .locals
            .iter()
            .rposition(|(symbol, _, _)| *symbol == variable);
        let target = match existing {
            Some(index) if self.locals[index].1 != sequence.element => {
                return Err(Refusal(
                    "a loop variable assigned values of different types",
                ));
            }
            Some(index) => self.locals[index].2,
            None => {
                let target = emitter
                    .as_mut()
                    .map(|emitter| emitter.builder.declare_var(machine_type(sequence.element)));
                self.locals.push((variable, sequence.element, target));
                target
            }
        };
        let blocks = match emitter.as_mut() {
            Some(emitter) => {
                let count = sequence
                    .count
                    .ok_or(Refusal("a sequence without a length"))?;
                let target = target.ok_or(Refusal("a loop variable without a variable"))?;
                let header = emitter.builder.create_block();
                let body_block = emitter.builder.create_block();
                let next = emitter.builder.create_block();
                let exit = emitter.builder.create_block();
                let index = emitter.builder.declare_var(I64);
                let zero = emitter.builder.ins().iconst(I64, 0);
                emitter.builder.def_var(index, zero);
                let mark = self.allocates.then(|| emitter.handle_mark());
                emitter.builder.ins().jump(header, &[]);
                emitter.builder.switch_to_block(header);
                let current = emitter.builder.use_var(index);
                let more = emitter
                    .builder
                    .ins()
                    .icmp(IntCC::SignedLessThan, current, count);
                emitter.builder.ins().brif(more, body_block, &[], exit, &[]);
                emitter.builder.switch_to_block(body_block);
                let value = match sequence.source {
                    Source::Range { start, step } => {
                        let offset = emitter.builder.ins().ireduce(I32, current);
                        let offset = emitter.builder.ins().imul(offset, step);
                        emitter.builder.ins().iadd(start, offset)
                    }
                    Source::Vector(vector) => {
                        let ty = sequence.element;
                        emitter.load_element(ty, vector, current)
                    }
                    Source::Typing => return Err(Refusal("a sequence without a value")),
                };
                emitter.builder.def_var(target, value);
                emitter.loops.push((next, exit));
                Some((index, header, next, exit, mark))
            }
            None => None,
        };
        self.breaks.push(false);
        self.loop_scopes.push((scope, false));
        self.statement = true;
        let walked = self.expression(emitter, body);
        self.breaks.pop();
        let carries = self.loop_scopes.pop().is_some_and(|(_, carries)| carries);
        self.locals.truncate(scope);
        walked?;
        if let (Some(emitter), Some((index, header, next, exit, mark))) = (emitter.as_mut(), blocks)
        {
            emitter.loops.pop();
            emitter.builder.ins().jump(next, &[]);
            emitter.builder.switch_to_block(next);
            if let Some(mark) = mark {
                self.release_iteration(emitter, mark, carries);
            }
            emitter.check_interrupt();
            let current = emitter.builder.use_var(index);
            let following = emitter.builder.ins().iadd_imm_s(current, 1);
            emitter.builder.def_var(index, following);
            emitter.builder.ins().jump(header, &[]);
            emitter.builder.switch_to_block(exit);
        }
        Ok(Val {
            kind: Kind::Known(Ty::Bottom),
            value: None,
        })
    }

    /// Releases what an iteration allocated, at a loop's back edge. When the iteration stored a
    /// record in a local from before the loop, that local's current record must survive, so
    /// every record local still in scope is rooted again above the mark; the handle stack
    /// then stays the same size across iterations.
    fn release_iteration(&self, emitter: &mut Emitter<'_>, mark: Value, carries: bool) {
        if !carries {
            emitter.release_handles(mark, None);
            return;
        }
        let mut kept = self
            .locals
            .iter()
            .filter(|(_, ty, _)| ty.is_record())
            .filter_map(|&(_, _, variable)| variable);
        let first = kept
            .next()
            .map(|variable| emitter.builder.use_var(variable));
        emitter.release_handles(mark, first);
        for variable in kept {
            let value = emitter.builder.use_var(variable);
            let top = emitter.handle_mark();
            emitter.release_handles(top, Some(value));
        }
    }

    /// `while (condition) body`, or `repeat body` without a condition, as a statement.
    unsafe fn loop_body(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        condition: Option<Sexp>,
        body: Sexp,
    ) -> Result<Val, Refusal> {
        let scope = self.locals.len();
        // Every iteration starts at the header, which checks for an interrupt first. `next`
        // goes to the latch, which releases what the iteration allocated.
        let allocates = self.allocates;
        let blocks = emitter.as_mut().map(|emitter| {
            let header = emitter.builder.create_block();
            let latch = emitter.builder.create_block();
            let exit = emitter.builder.create_block();
            let mark = allocates.then(|| emitter.handle_mark());
            emitter.builder.ins().jump(header, &[]);
            emitter.builder.switch_to_block(header);
            emitter.check_interrupt();
            emitter.loops.push((latch, exit));
            (header, latch, exit, mark)
        });
        self.breaks.push(false);
        self.loop_scopes.push((scope, false));
        let walked = self.loop_iteration(emitter, condition, body);
        let breaks = self.breaks.pop().unwrap_or(true);
        let carries = self.loop_scopes.pop().is_some_and(|(_, carries)| carries);
        self.locals.truncate(scope);
        let terminates = walked? || breaks;
        if let (Some(emitter), Some((header, latch, exit, mark))) = (emitter.as_mut(), blocks) {
            emitter.loops.pop();
            emitter.builder.ins().jump(latch, &[]);
            emitter.builder.switch_to_block(latch);
            if let Some(mark) = mark {
                self.release_iteration(emitter, mark, carries);
            }
            emitter.builder.ins().jump(header, &[]);
            emitter.builder.switch_to_block(exit);
        }
        // A loop that cannot end other than through `return` never completes.
        let kind = if terminates {
            Kind::Known(Ty::Bottom)
        } else {
            Kind::Never
        };
        Ok(Val { kind, value: None })
    }

    /// One iteration of a `while` or `repeat` loop: whether a condition may end the loop.
    unsafe fn loop_iteration(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        condition: Option<Sexp>,
        body: Sexp,
    ) -> Result<bool, Refusal> {
        if let Some(condition) = condition {
            let condition = self.expression(emitter, condition)?;
            if condition.kind == Kind::Never {
                return Ok(false);
            }
            let ty = condition.kind.numeric()?;
            if let Some(emitter) = emitter.as_mut() {
                let ty = ty.ok_or(Refusal("a condition of undetermined type"))?;
                let value = condition
                    .value
                    .ok_or(Refusal("a condition without a value"))?;
                let truth = emitter.truth(ty, value);
                let body_block = emitter.builder.create_block();
                let &(_, exit) = emitter
                    .loops
                    .last()
                    .ok_or(Refusal("a loop without blocks"))?;
                emitter
                    .builder
                    .ins()
                    .brif(truth, body_block, &[], exit, &[]);
                emitter.builder.switch_to_block(body_block);
            }
        }
        self.statement = true;
        self.expression(emitter, body)?;
        Ok(condition.is_some())
    }

    /// The values a `for` loop runs over, evaluated once before it starts. `None` when
    /// evaluating them never completes.
    unsafe fn sequence(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        expression: Sexp,
    ) -> Result<Option<Sequence>, Refusal> {
        let generator = if expression.kind() == SexpType::Language
            && expression.car().kind() == SexpType::Symbol
            && !self
                .locals
                .iter()
                .any(|(symbol, _, _)| *symbol == expression.car())
        {
            let speculation = resolve_function(expression.car(), self.environment)
                .ok_or(Refusal("a function the lookup cannot resolve"))?;
            let operation = base_builtin(speculation.function, expression.car())
                .and_then(|name| Operation::of(&name))
                .filter(|operation| {
                    matches!(
                        operation,
                        Operation::SeqLen | Operation::SeqAlong | Operation::Colon
                    )
                });
            if operation.is_some() {
                self.speculations.push(speculation);
            }
            operation
        } else {
            None
        };
        let Some(generator) = generator else {
            let vector = self.expression(emitter, expression)?;
            let ty = match vector.kind {
                Kind::Never => return Ok(None),
                Kind::Known(ty) if ty.is_vector() => ty,
                _ => {
                    return Err(Refusal(
                        "a loop over something other than a vector or a sequence",
                    ));
                }
            };
            let (count, source) = match (emitter.as_mut(), vector.value) {
                (Some(emitter), Some(vector)) => {
                    (Some(emitter.vector_length(vector)), Source::Vector(vector))
                }
                _ => (None, Source::Typing),
            };
            return Ok(Some(Sequence {
                element: ty.element(),
                count,
                source,
            }));
        };
        let args: SmallVec<[Sexp; 2]> = values::pairlist_items(expression.cdr())
            .iter()
            .map(|&(tag, value)| {
                if tag != values::nil() || value == values::missing_arg() {
                    Err(Refusal("a sequence with named or missing arguments"))
                } else {
                    Ok(value)
                }
            })
            .collect::<Result<_, _>>()?;
        let mut operands: SmallVec<[(Ty, Option<Value>); 2]> = SmallVec::new();
        for &arg in &args {
            let operand = self.expression(emitter, arg)?;
            match operand.kind {
                Kind::Never => return Ok(None),
                Kind::Known(ty) if ty.is_vector() && generator == Operation::SeqAlong => {
                    operands.push((ty, operand.value));
                }
                _ => {
                    let ty = operand
                        .kind
                        .numeric()?
                        .ok_or(Refusal("a sequence of undetermined type"))?;
                    operands.push((ty, operand.value));
                }
            }
        }
        let Some(emitter) = emitter.as_mut() else {
            if !matches!((generator, operands.len()), (Operation::Colon, 2) | (_, 1)) {
                return Err(Refusal("a sequence call native code lacks"));
            }
            return Ok(Some(Sequence {
                element: Ty::Int,
                count: None,
                source: Source::Typing,
            }));
        };
        let value = |(ty, value): (Ty, Option<Value>)| {
            value
                .map(|value| (ty, value))
                .ok_or(Refusal("a sequence operand without a value"))
        };
        let one = emitter.builder.ins().iconst(I32, 1);
        let (start, step, count) = match (generator, operands.as_slice()) {
            (Operation::SeqLen, &[length]) => {
                let (ty, length) = value(length)?;
                (one, one, emitter.sequence_length(ty, length))
            }
            (Operation::SeqAlong, &[along]) => {
                let (ty, along) = value(along)?;
                let count = if ty.is_vector() {
                    let length = emitter.vector_length(along);
                    // A longer vector's indexes are doubles.
                    let long = emitter.builder.ins().icmp_imm_s(
                        IntCC::SignedGreaterThan,
                        length,
                        i64::from(i32::MAX),
                    );
                    emitter.fail_if(long);
                    length
                } else {
                    emitter.builder.ins().iconst(I64, 1)
                };
                (one, one, count)
            }
            (Operation::Colon, &[from, to]) => {
                let (from_ty, from) = value(from)?;
                let (to_ty, to) = value(to)?;
                emitter.colon((from_ty, from), (to_ty, to))
            }
            _ => return Err(Refusal("a sequence call native code lacks")),
        };
        Ok(Some(Sequence {
            element: Ty::Int,
            count: Some(count),
            source: Source::Range { start, step },
        }))
    }

    /// `left && right` or `left || right`. A missing left operand, whose result depends on the
    /// right one in a way native code does not reproduce, makes the function fail.
    unsafe fn short_circuit(
        &mut self,
        emitter: &mut Emitting<'_, '_>,
        and: bool,
        left: Sexp,
        right: Sexp,
    ) -> Result<Val, Refusal> {
        let left = self.expression(emitter, left)?;
        if left.kind == Kind::Never {
            return Ok(left);
        }
        let left_ty = left.kind.numeric()?;
        let join = match (emitter.as_mut(), left.value) {
            (Some(emitter), Some(value)) => {
                let ty = left_ty.ok_or(Refusal("an operand of undetermined type"))?;
                let truth = emitter.truth(ty, value);
                let right_label = emitter.builder.create_block();
                let join = emitter.builder.create_block();
                emitter.builder.append_block_param(join, I32);
                let decided = emitter.builder.ins().iconst(I32, if and { 0 } else { 1 });
                if and {
                    emitter
                        .builder
                        .ins()
                        .brif(truth, right_label, &[], join, &[decided.into()]);
                } else {
                    emitter
                        .builder
                        .ins()
                        .brif(truth, join, &[decided.into()], right_label, &[]);
                }
                emitter.builder.switch_to_block(right_label);
                Some(join)
            }
            (Some(_), None) => return Err(Refusal("an operand without a value")),
            (None, _) => None,
        };
        let scope = self.locals.len();
        let right = self.expression(emitter, right)?;
        self.locals.truncate(scope);
        if right.kind != Kind::Never {
            right.kind.numeric()?;
        }
        let value = match (emitter.as_mut(), join) {
            (Some(emitter), Some(join)) => {
                match (right.kind, right.value) {
                    (Kind::Never, _) => {
                        let failure = emitter.failure;
                        emitter.builder.ins().jump(failure, &[]);
                    }
                    (Kind::Known(ty), Some(value)) => {
                        let logical = emitter.logical(ty, value);
                        emitter.builder.ins().jump(join, &[logical.into()]);
                    }
                    _ => return Err(Refusal("an operand without a value")),
                }
                emitter.builder.switch_to_block(join);
                emitter.builder.block_params(join).first().copied()
            }
            _ => None,
        };
        Ok(Val {
            kind: Kind::Known(Ty::Lgl),
            value,
        })
    }
}

/// The values a `for` loop runs over: `count` of them, of type `element`.
struct Sequence {
    element: Ty,
    count: Option<Value>,
    source: Source,
}

#[derive(Clone, Copy)]
enum Source {
    /// The integers `start`, `start + step`, and so on.
    Range { start: Value, step: Value },
    /// The elements of a vector.
    Vector(Value),
    /// A walk that only types the body.
    Typing,
}

/// Whether a native function's body is small enough to compile into its callers, and has no
/// `return`, which would leave the caller instead.
unsafe fn inlinable(body: Sexp) -> bool {
    unsafe fn visit(expression: Sexp, budget: &mut usize) -> bool {
        if expression.kind() != SexpType::Language {
            return true;
        }
        if *budget == 0 || expression.car() == values::symbols().return_ {
            return false;
        }
        *budget -= 1;
        values::pairlist_items(expression.cdr())
            .iter()
            .all(|&(_, argument)| visit(argument, budget))
    }
    let mut budget = MAX_INLINE_SIZE;
    visit(body, &mut budget)
}

/// Emits the body of a native function into `builder`.
unsafe fn emit<'f>(
    builder: FunctionBuilder<'f>,
    call_conv: CallConv,
    own: FuncRef,
    walker: &mut Walker<'_>,
    body: Sexp,
) -> Result<FunctionBuilder<'f>, Refusal> {
    let mut builder = builder;
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let params: SmallVec<[Value; 5]> = builder.block_params(entry).iter().copied().collect();
    let (&budget, arguments) = params
        .split_first()
        .ok_or(Refusal("a native function without a budget"))?;
    let failure = builder.create_block();
    builder.set_cold_block(failure);
    let result = walker.own_result.unwrap_or(Ty::Int);
    let mut emitter = Emitter {
        builder,
        call_conv,
        own,
        budget,
        failure,
        loops: Vec::new(),
        mark: None,
        returns_record: result.is_record(),
    };
    if walker.allocates {
        emitter.mark = Some(emitter.handle_mark());
    }
    // A call nested deeper than R allows, or a pending interrupt, belongs to the interpreter.
    let exhausted = emitter
        .builder
        .ins()
        .icmp_imm_s(IntCC::SignedLessThanOrEqual, budget, 0);
    emitter.fail_if(exhausted);
    emitter.check_interrupt();
    for (local, &argument) in walker.locals.iter_mut().zip(arguments) {
        let variable = emitter.builder.declare_var(machine_type(local.1));
        emitter.builder.def_var(variable, argument);
        local.2 = Some(variable);
    }
    let value = walker.walk(Some(&mut emitter), body)?;
    if let Some(value) = value.value {
        emitter.finish(value);
    }
    emitter.builder.ins().jump(failure, &[]);
    emitter.builder.switch_to_block(failure);
    if let Some(mark) = emitter.mark {
        emitter.release_handles(mark, None);
    }
    let status = emitter.builder.ins().iconst(I64, FAILED);
    let zero = match machine_type(result) {
        F64 => emitter.builder.ins().f64const(0.0),
        I64 => emitter.builder.ins().iconst(I64, 0),
        _ => emitter.builder.ins().iconst(I32, 0),
    };
    emitter.builder.ins().return_(&[status, zero]);
    Ok(emitter.builder)
}

/// The native function a call reaches.
#[derive(Clone, Copy)]
enum Callee {
    Own(FuncRef),
    Other(&'static Native),
}

struct Emitter<'f> {
    builder: FunctionBuilder<'f>,
    call_conv: CallConv,
    /// The function being compiled, for its recursive calls.
    own: FuncRef,
    /// How many calls may still nest below this one.
    budget: Value,
    /// The block that returns [`FAILED`].
    failure: cranelift_codegen::ir::Block,
    /// The enclosing loops' blocks for `next` and `break`, innermost last.
    loops: Vec<(cranelift_codegen::ir::Block, cranelift_codegen::ir::Block)>,
    /// Where the handle stack stood on entry, for a function that allocates.
    mark: Option<Value>,
    /// Whether the function returns a record, which stays rooted when it returns.
    returns_record: bool,
}

impl Emitter<'_> {
    /// Fails when the interrupt flag is raised, so the interpreter runs the call and its polls
    /// decide whether to deliver a pending interrupt.
    fn check_interrupt(&mut self) {
        let flag = &interp::unwind::INTERRUPT_REQUESTED as *const AtomicBool as i64;
        let address = self.builder.ins().iconst(I64, flag);
        let requested = self
            .builder
            .ins()
            .uload8(I32, MemFlagsData::trusted(), address, 0);
        self.fail_if(requested);
    }

    fn vector_length(&mut self, vector: Value) -> Value {
        self.builder
            .ins()
            .load(I64, MemFlagsData::trusted(), vector, codegen::LENGTH)
    }

    /// `length(vector)`, failing for a vector whose length is a double.
    fn length(&mut self, vector: Value) -> Value {
        let length = self.vector_length(vector);
        let long =
            self.builder
                .ins()
                .icmp_imm_s(IntCC::SignedGreaterThan, length, i64::from(i32::MAX));
        self.fail_if(long);
        self.builder.ins().ireduce(I32, length)
    }

    /// The element at zero-based `offset` of a vector of `ty`s.
    fn load_element(&mut self, ty: Ty, vector: Value, offset: Value) -> Value {
        let width = if ty == Ty::Dbl { 8 } else { 4 };
        let scaled = self.builder.ins().imul_imm_s(offset, width);
        let address = self.builder.ins().iadd(vector, scaled);
        self.builder.ins().load(
            machine_type(ty),
            MemFlagsData::trusted(),
            address,
            codegen::DATA,
        )
    }

    /// `vector[[index]]` of a vector of `ty`s, failing where the index is `NA` or out of range.
    /// A double index truncates toward zero, as R's does.
    fn element(&mut self, ty: Ty, vector: Value, index_ty: Ty, index: Value) -> Value {
        let length = self.vector_length(vector);
        let index = if index_ty == Ty::Dbl {
            let one = self.builder.ins().f64const(1.0);
            let below = self
                .builder
                .ins()
                .fcmp(FloatCC::GreaterThanOrEqual, index, one);
            let bound = self.builder.ins().fcvt_from_sint(F64, length);
            let bound = self.builder.ins().fadd(bound, one);
            let within = self.builder.ins().fcmp(FloatCC::LessThan, index, bound);
            let in_range = self.builder.ins().band(below, within);
            let outside = self.builder.ins().bxor_imm_u(in_range, 1);
            self.fail_if(outside);
            self.builder.ins().fcvt_to_sint_sat(I64, index)
        } else {
            let index = self.builder.ins().sextend(I64, index);
            let below = self
                .builder
                .ins()
                .icmp_imm_s(IntCC::SignedLessThan, index, 1);
            let beyond = self
                .builder
                .ins()
                .icmp(IntCC::SignedGreaterThan, index, length);
            let outside = self.builder.ins().bor(below, beyond);
            self.fail_if(outside);
            index
        };
        let offset = self.builder.ins().iadd_imm_s(index, -1);
        self.load_element(ty, vector, offset)
    }

    /// The length of `seq_len(length)`, failing where R raises an error or the sequence's
    /// values would be doubles.
    fn sequence_length(&mut self, ty: Ty, length: Value) -> Value {
        if ty == Ty::Dbl {
            let zero = self.builder.ins().f64const(0.0);
            let negative = self
                .builder
                .ins()
                .fcmp(FloatCC::UnorderedOrLessThan, length, zero);
            let limit = self.builder.ins().f64const(2147483648.0);
            let long = self
                .builder
                .ins()
                .fcmp(FloatCC::GreaterThanOrEqual, length, limit);
            let bad = self.builder.ins().bor(negative, long);
            self.fail_if(bad);
            self.builder.ins().fcvt_to_sint_sat(I64, length)
        } else {
            // `NA` is the smallest integer, so it fails as a negative length.
            let negative = self
                .builder
                .ins()
                .icmp_imm_s(IntCC::SignedLessThan, length, 0);
            self.fail_if(negative);
            self.builder.ins().sextend(I64, length)
        }
    }

    /// `from:to` as `(start, step, count)`, failing where R raises an error or the values would
    /// be doubles, following R's own rule: the sequence is integer when `from` is a whole number
    /// in integer range and so is the last value.
    fn colon(
        &mut self,
        (from_ty, from): (Ty, Value),
        (to_ty, to): (Ty, Value),
    ) -> (Value, Value, Value) {
        let from = self.as_double(from_ty, from);
        let to = self.as_double(to_ty, to);
        let missing = self.builder.ins().fcmp(FloatCC::Unordered, from, to);
        self.fail_if(missing);
        let whole = self.builder.ins().trunc(from);
        let fractional = self.builder.ins().fcmp(FloatCC::NotEqual, whole, from);
        let distance = self.builder.ins().fsub(to, from);
        let distance = self.builder.ins().fabs(distance);
        let one = self.builder.ins().f64const(1.0);
        let epsilon = self.builder.ins().f64const(f64::from(f32::EPSILON));
        let count = self.builder.ins().fadd(distance, one);
        let count = self.builder.ins().fadd(count, epsilon);
        let count = self.builder.ins().floor(count);
        let span = self.builder.ins().fsub(count, one);
        let ascending = self.builder.ins().fcmp(FloatCC::LessThanOrEqual, from, to);
        let back = self.builder.ins().fneg(span);
        let span = self.builder.ins().select(ascending, span, back);
        let last = self.builder.ins().fadd(from, span);
        let low = self.builder.ins().f64const(f64::from(i32::MIN));
        let high = self.builder.ins().f64const(f64::from(i32::MAX));
        let mut bad = fractional;
        for value in [from, last] {
            let under = self
                .builder
                .ins()
                .fcmp(FloatCC::LessThanOrEqual, value, low);
            let over = self.builder.ins().fcmp(FloatCC::GreaterThan, value, high);
            let outside = self.builder.ins().bor(under, over);
            bad = self.builder.ins().bor(bad, outside);
        }
        self.fail_if(bad);
        let start = self.builder.ins().fcvt_to_sint_sat(I32, from);
        let up = self.builder.ins().iconst(I32, 1);
        let down = self.builder.ins().iconst(I32, -1);
        let step = self.builder.ins().select(ascending, up, down);
        let count = self.builder.ins().fcvt_to_sint_sat(I64, count);
        (start, step, count)
    }

    /// Calls a native function, failing when it fails.
    fn call(&mut self, callee: Callee, arguments: &[Value]) -> Result<Value, Refusal> {
        let budget = self.builder.ins().iadd_imm_s(self.budget, -1);
        let mut args: SmallVec<[Value; 5]> = SmallVec::new();
        args.push(budget);
        args.extend_from_slice(arguments);
        let call = match callee {
            Callee::Own(reference) => self.builder.ins().call(reference, &args),
            Callee::Other(native) => {
                let signature = self.builder.import_signature(signature(
                    &native.params,
                    native.result,
                    self.call_conv,
                ));
                let address = self.builder.ins().iconst(I64, native.address as i64);
                self.builder.ins().call_indirect(signature, address, &args)
            }
        };
        let &[status, value] = self.builder.inst_results(call) else {
            return Err(Refusal("a native call without a status and a value"));
        };
        let failed = self.builder.ins().icmp_imm_s(IntCC::NotEqual, status, OK);
        self.fail_if(failed);
        Ok(value)
    }

    /// Returns `value` and continues in a block nothing reaches.
    fn finish(&mut self, value: Value) {
        if let Some(mark) = self.mark {
            let keep = self.returns_record.then_some(value);
            self.release_handles(mark, keep);
        }
        let status = self.builder.ins().iconst(I64, OK);
        self.builder.ins().return_(&[status, value]);
        let dead = self.builder.create_block();
        self.builder.switch_to_block(dead);
    }

    /// Where the handle stack stands.
    fn handle_mark(&mut self) -> Value {
        self.helper(
            runtime::jit_native_mark as *const () as usize,
            &[],
            I64,
            &[],
        )
    }

    /// Releases every handle rooted since `mark`, then roots `keep`.
    fn release_handles(&mut self, mark: Value, keep: Option<Value>) {
        let keep = keep.unwrap_or_else(|| self.builder.ins().iconst(I64, 0));
        let mut signature = Signature::new(self.call_conv);
        signature.params.push(AbiParam::new(I64));
        signature.params.push(AbiParam::new(I64));
        let signature = self.builder.import_signature(signature);
        let callee = self
            .builder
            .ins()
            .iconst(I64, runtime::jit_native_release as *const () as i64);
        self.builder
            .ins()
            .call_indirect(signature, callee, &[mark, keep]);
    }

    /// Slot `index` of `record`, failing unless it holds a value of type `ty`: an unboxed scalar,
    /// or for [`Ty::Obj`] a handle to a plain record.
    fn field(&mut self, ty: Ty, record: Value, index: usize) -> Result<Value, Refusal> {
        let offset = i32::try_from(RECORD_DATA_OFFSET as u64 + index as u64 * 8)
            .map_err(|_| Refusal("a record field past the addressable slots"))?;
        let raw = self
            .builder
            .ins()
            .load(I64, MemFlagsData::trusted(), record, offset);
        let unboxed = if index < UNBOXED_SLOTS {
            let mask = self.builder.ins().load(
                I64,
                MemFlagsData::trusted(),
                record,
                RECORD_UNBOXED_OFFSET as i32,
            );
            let bit = self.builder.ins().band_imm_u(mask, 1i64 << index);
            self.builder.ins().icmp_imm_s(IntCC::NotEqual, bit, 0)
        } else {
            self.builder
                .ins()
                .iconst(cranelift_codegen::ir::types::I8, 0)
        };
        if ty == Ty::Obj {
            self.fail_if(unboxed);
            let kind = self
                .builder
                .ins()
                .uload8(I32, MemFlagsData::trusted(), raw, codegen::KIND);
            let other =
                self.builder
                    .ins()
                    .icmp_imm_s(IntCC::NotEqual, kind, SexpType::Record as i64);
            let bits = self
                .builder
                .ins()
                .uload8(I32, MemFlagsData::trusted(), raw, codegen::FLAGS);
            let object = self.builder.ins().band_imm_u(bits, codegen::OBJECT_FLAG);
            let classed = self.builder.ins().icmp_imm_s(IntCC::NotEqual, object, 0);
            let unusable = self.builder.ins().bor(other, classed);
            self.fail_if(unusable);
            return Ok(raw);
        }
        let upper = self.builder.ins().ushr_imm_u(raw, 32);
        let integer = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, upper, i64::from(INTEGER_PREFIX));
        let logical = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, upper, i64::from(LOGICAL_PREFIX));
        let fits = match ty {
            Ty::Int => integer,
            Ty::Lgl => logical,
            _ => {
                let either = self.builder.ins().bor(integer, logical);
                self.builder.ins().icmp_imm_s(IntCC::Equal, either, 0)
            }
        };
        let usable = self.builder.ins().band(unboxed, fits);
        let unusable = self.builder.ins().bxor_imm_u(usable, 1);
        self.fail_if(unusable);
        Ok(if ty == Ty::Dbl {
            self.builder.ins().bitcast(F64, MemFlagsData::new(), raw)
        } else {
            self.builder.ins().ireduce(I32, raw)
        })
    }

    fn fail_if(&mut self, condition: Value) {
        let next = self.builder.create_block();
        self.builder
            .ins()
            .brif(condition, self.failure, &[], next, &[]);
        self.builder.switch_to_block(next);
    }

    /// Ends a branch of an `if` by jumping to `join`, carrying its value when it is `used`.
    fn close_branch(
        &mut self,
        join: cranelift_codegen::ir::Block,
        branch: Val,
        used: bool,
    ) -> Result<(), Refusal> {
        if branch.kind == Kind::Never {
            // The branch returned: the block it ends in is unreachable.
            self.builder.ins().jump(self.failure, &[]);
            return Ok(());
        }
        if !used {
            self.builder.ins().jump(join, &[]);
            return Ok(());
        }
        let value = branch.value.ok_or(Refusal("a branch without a value"))?;
        if self.builder.block_params(join).is_empty() {
            let ty = self.builder.func.dfg.value_type(value);
            self.builder.append_block_param(join, ty);
        }
        self.builder.ins().jump(join, &[value.into()]);
        Ok(())
    }

    /// `max(x, y)` or `min(x, y)`, failing on `NA` and `NaN`, whose propagation native code
    /// leaves to the interpreter. A tie keeps `x`, as R's does.
    fn extreme(
        &mut self,
        maximum: bool,
        integers: bool,
        (x_ty, x): (Ty, Value),
        (y_ty, y): (Ty, Value),
    ) -> Value {
        if integers {
            for value in [x, y] {
                let missing = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, value, NA_INTEGER as i64);
                self.fail_if(missing);
            }
            let condition = if maximum {
                IntCC::SignedGreaterThan
            } else {
                IntCC::SignedLessThan
            };
            let wins = self.builder.ins().icmp(condition, y, x);
            return self.builder.ins().select(wins, y, x);
        }
        let x = self.as_double(x_ty, x);
        let y = self.as_double(y_ty, y);
        let unordered = self.builder.ins().fcmp(FloatCC::Unordered, x, y);
        self.fail_if(unordered);
        let condition = if maximum {
            FloatCC::GreaterThan
        } else {
            FloatCC::LessThan
        };
        let wins = self.builder.ins().fcmp(condition, y, x);
        self.builder.ins().select(wins, y, x)
    }

    fn negate(&mut self, ty: Ty, value: Value) -> Value {
        if ty == Ty::Dbl {
            self.builder.ins().fneg(value)
        } else {
            // `NA` is the smallest integer, which negation wraps to itself.
            self.builder.ins().ineg(value)
        }
    }

    /// `!` of an integer or a logical.
    fn not(&mut self, value: Value) -> Value {
        let missing = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, value, NA_INTEGER as i64);
        let zero = self.builder.ins().icmp_imm_s(IntCC::Equal, value, 0);
        let zero = self.builder.ins().uextend(I32, zero);
        self.builder.ins().select(missing, value, zero)
    }

    fn as_double(&mut self, ty: Ty, value: Value) -> Value {
        if ty == Ty::Dbl {
            return value;
        }
        let missing = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, value, NA_INTEGER as i64);
        let converted = self.builder.ins().fcvt_from_sint(F64, value);
        let na = self.builder.ins().f64const(Ieee64::with_bits(NA_REAL_BITS));
        self.builder.ins().select(missing, na, converted)
    }

    /// A number as a logical: `NA` stays `NA`, zero is `FALSE` and anything else `TRUE`.
    fn logical(&mut self, ty: Ty, value: Value) -> Value {
        let na = self.builder.ins().iconst(I32, NA_INTEGER as i64);
        let (missing, nonzero) = if ty == Ty::Dbl {
            let zero = self.builder.ins().f64const(0.0);
            (
                self.builder.ins().fcmp(FloatCC::Unordered, value, value),
                self.builder.ins().fcmp(FloatCC::NotEqual, value, zero),
            )
        } else {
            (
                self.builder.ins().icmp(IntCC::Equal, value, na),
                self.builder.ins().icmp_imm_s(IntCC::NotEqual, value, 0),
            )
        };
        let nonzero = self.builder.ins().uextend(I32, nonzero);
        self.builder.ins().select(missing, na, nonzero)
    }

    /// A condition's truth, failing on `NA`, where R raises an error.
    fn truth(&mut self, ty: Ty, value: Value) -> Value {
        if ty == Ty::Dbl {
            let missing = self.builder.ins().fcmp(FloatCC::Unordered, value, value);
            self.fail_if(missing);
            let zero = self.builder.ins().f64const(0.0);
            self.builder.ins().fcmp(FloatCC::NotEqual, value, zero)
        } else {
            let missing = self
                .builder
                .ins()
                .icmp_imm_s(IntCC::Equal, value, NA_INTEGER as i64);
            self.fail_if(missing);
            self.builder.ins().icmp_imm_s(IntCC::NotEqual, value, 0)
        }
    }

    fn helper(&mut self, address: usize, params: &[Type], result: Type, args: &[Value]) -> Value {
        let mut signature = Signature::new(self.call_conv);
        signature
            .params
            .extend(params.iter().map(|&ty| AbiParam::new(ty)));
        signature.returns.push(AbiParam::new(result));
        let signature = self.builder.import_signature(signature);
        let callee = self.builder.ins().iconst(I64, address as i64);
        let call = self.builder.ins().call_indirect(signature, callee, args);
        // The signature declares exactly one result.
        self.builder.inst_results(call)[0]
    }

    /// A one-argument math function, its operand of type `ty`.
    fn math(&mut self, operation: Operation, ty: Ty, value: Value) -> Value {
        if operation == Operation::Abs {
            return if ty == Ty::Dbl {
                self.builder.ins().fabs(value)
            } else {
                // `NA` is the smallest integer, whose absolute value wraps to itself.
                self.builder.ins().iabs(value)
            };
        }
        let value = self.as_double(ty, value);
        match operation {
            Operation::Sqrt | Operation::Log => {
                // A negative argument warns.
                let zero = self.builder.ins().f64const(0.0);
                let negative = self.builder.ins().fcmp(FloatCC::LessThan, value, zero);
                self.fail_if(negative);
                if operation == Operation::Sqrt {
                    self.builder.ins().sqrt(value)
                } else {
                    self.helper(
                        runtime::jit_log as *const () as usize,
                        &[F64],
                        F64,
                        &[value],
                    )
                }
            }
            Operation::Exp => self.helper(
                runtime::jit_exp as *const () as usize,
                &[F64],
                F64,
                &[value],
            ),
            Operation::Floor => self.builder.ins().floor(value),
            _ => self.builder.ins().ceil(value),
        }
    }

    /// A binary arithmetic operation or comparison. `integers` when both operands compute as
    /// integers.
    fn binary(
        &mut self,
        operation: Operation,
        integers: bool,
        (a, x): (Ty, Value),
        (b, y): (Ty, Value),
    ) -> Value {
        let na = self.builder.ins().iconst(I32, NA_INTEGER as i64);
        if operation.is_comparison() {
            let (missing, holds) = if integers {
                let x_na = self.builder.ins().icmp(IntCC::Equal, x, na);
                let y_na = self.builder.ins().icmp(IntCC::Equal, y, na);
                let condition = match operation {
                    Operation::Less => IntCC::SignedLessThan,
                    Operation::Greater => IntCC::SignedGreaterThan,
                    Operation::LessEqual => IntCC::SignedLessThanOrEqual,
                    Operation::GreaterEqual => IntCC::SignedGreaterThanOrEqual,
                    Operation::Equal => IntCC::Equal,
                    _ => IntCC::NotEqual,
                };
                (
                    self.builder.ins().bor(x_na, y_na),
                    self.builder.ins().icmp(condition, x, y),
                )
            } else {
                let x = self.as_double(a, x);
                let y = self.as_double(b, y);
                let condition = match operation {
                    Operation::Less => FloatCC::LessThan,
                    Operation::Greater => FloatCC::GreaterThan,
                    Operation::LessEqual => FloatCC::LessThanOrEqual,
                    Operation::GreaterEqual => FloatCC::GreaterThanOrEqual,
                    Operation::Equal => FloatCC::Equal,
                    _ => FloatCC::NotEqual,
                };
                (
                    self.builder.ins().fcmp(FloatCC::Unordered, x, y),
                    self.builder.ins().fcmp(condition, x, y),
                )
            };
            let holds = self.builder.ins().uextend(I32, holds);
            return self.builder.ins().select(missing, na, holds);
        }
        if integers {
            match operation {
                Operation::Modulo | Operation::IntegerDivide => {
                    let helper = if operation == Operation::Modulo {
                        runtime::jit_integer_modulo as *const () as usize
                    } else {
                        runtime::jit_integer_divide as *const () as usize
                    };
                    return self.helper(helper, &[I32, I32], I32, &[x, y]);
                }
                Operation::Add | Operation::Subtract | Operation::Multiply => {
                    let (value, overflow) = match operation {
                        Operation::Add => self.builder.ins().sadd_overflow(x, y),
                        Operation::Subtract => self.builder.ins().ssub_overflow(x, y),
                        _ => self.builder.ins().smul_overflow(x, y),
                    };
                    let x_na = self.builder.ins().icmp(IntCC::Equal, x, na);
                    let y_na = self.builder.ins().icmp(IntCC::Equal, y, na);
                    let any_na = self.builder.ins().bor(x_na, y_na);
                    let result_na = self.builder.ins().icmp(IntCC::Equal, value, na);
                    let bad = self.builder.ins().bor(overflow, result_na);
                    // An `NA` operand gives `NA`; otherwise an overflow warns.
                    let not_na = self.builder.ins().bxor_imm_u(any_na, 1);
                    let warns = self.builder.ins().band(bad, not_na);
                    self.fail_if(warns);
                    return self.builder.ins().select(any_na, na, value);
                }
                _ => {}
            }
        }
        let x = self.as_double(a, x);
        let y = self.as_double(b, y);
        match operation {
            Operation::Add => self.builder.ins().fadd(x, y),
            Operation::Subtract => self.builder.ins().fsub(x, y),
            Operation::Multiply => self.builder.ins().fmul(x, y),
            Operation::Divide => self.builder.ins().fdiv(x, y),
            Operation::Power => self.helper(
                runtime::jit_power as *const () as usize,
                &[F64, F64],
                F64,
                &[x, y],
            ),
            _ => {
                // Tier 0 raises the warning, so a modulus that loses accuracy fails over.
                let loses = self.helper(
                    runtime::jit_modulus_loses_accuracy as *const () as usize,
                    &[F64, F64],
                    I32,
                    &[x, y],
                );
                let loses = self.builder.ins().icmp_imm_u(IntCC::NotEqual, loses, 0);
                self.fail_if(loses);
                self.helper(
                    runtime::jit_real_modulo as *const () as usize,
                    &[F64, F64],
                    F64,
                    &[x, y],
                )
            }
        }
    }
}

/// Calls a typed closure through its native function when the arguments' values are at hand
/// without forcing anything and the closure compiles for their types: the call then needs no
/// frame, no promises and no annotation checks. `None` lets the caller make the call as usual,
/// which is also what happens when the native function fails, since it has done nothing anyone
/// can see by then.
pub unsafe fn call(function: Sexp, args: &[interp::builtins::Arg]) -> Option<R<Sexp>> {
    use heap::object::gp;
    if function.has_gp(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP | gp::CLOSURE_TRACE)
        || interp::moves::tracking()
    {
        return None;
    }
    let formals = function.car();
    let mut formal = formals;
    let mut types: SmallVec<[Ty; 4]> = SmallVec::new();
    let mut words: SmallVec<[u64; 4]> = SmallVec::new();
    let mut read: SmallVec<[(Sexp, Sexp); 4]> = SmallVec::new();
    for arg in args {
        if formal.kind() != SexpType::Pairlist
            || formal.tag() == values::symbols().dots
            || arg.tag != values::nil() && arg.tag != formal.tag()
        {
            return None;
        }
        let value = argument_value(arg.value)?;
        if arg.value.kind() == SexpType::Promise && arg.value.car() == values::unbound() {
            read.push((arg.value, value));
        }
        let (ty, word) = argument_word(value)?;
        types.push(ty);
        words.push(word);
        formal = formal.cdr();
    }
    if formal.kind() == SexpType::Pairlist {
        return None;
    }
    let native = native_for(function, &types)?;
    let trampoline = trampoline(&native.params, native.result)?;
    let heap = interp::heap();
    let mark = heap.handle_mark();
    let mut out = 0u64;
    let status = trampoline(
        native.address,
        runtime::jit_depth_budget(),
        words.as_ptr(),
        &mut out,
    );
    if status != OK {
        heap.release_handles(mark);
        return None;
    }
    let value = match native.result {
        Ty::Int => values::scalar_integer(out as u32 as i32),
        Ty::Lgl => values::scalar_logical(out as u32 as i32),
        Ty::Dbl => values::scalar_real(f64::from_bits(out)),
        _ => Sexp::from_raw(out as *mut heap::object::Header)
            .ok_or_else(|| interp::unwind::error("a native function returned no record")),
    };
    heap.release_handles(mark);
    // The call read each unforced argument promise, so it is forced now: a promise shared
    // through `...` must keep the value this call saw, as if tier 0 had forced it.
    for (promise, value) in read {
        heap.set_car(promise, value);
        heap.set_tag(promise, values::nil());
    }
    interp::interp().visible = true;
    Some(value)
}

type R<T> = Result<T, interp::unwind::Unwind>;

/// The value an argument has without evaluating anything: a constant, a forced promise's value,
/// or the binding a promise of a bare variable would read, unless reading it could run code.
unsafe fn argument_value(value: Sexp) -> Option<Sexp> {
    if value.kind() != SexpType::Promise {
        return (value != values::missing_arg()).then_some(value);
    }
    if value.car() != values::unbound() {
        return Some(value.car());
    }
    let symbol = value.cdr();
    if matches!(
        symbol.kind(),
        SexpType::Integer | SexpType::Real | SexpType::Logical
    ) {
        // A promise of a constant evaluates to the constant itself.
        return Some(symbol);
    }
    // A checked promise runs its annotation check when forced, which only tier 0 does.
    if symbol.kind() != SexpType::Symbol
        || value.tag() == values::nil()
        || value.has_gp(heap::object::gp::CHECKED_PROMISE)
    {
        return None;
    }
    let cell = interp::eval::find_cell(symbol, value.tag())?;
    if cell.has_gp(heap::object::gp::ACTIVE_BINDING) {
        return None;
    }
    let bound = cell.cdr();
    match bound.kind() {
        SexpType::Promise if bound.car() != values::unbound() => Some(bound.car()),
        SexpType::Promise => None,
        _ if bound == values::missing_arg() || bound == values::unbound() => None,
        _ => Some(bound),
    }
}

/// The native type of an argument's value and the 64 bits a native function takes it as.
unsafe fn argument_word(value: Sexp) -> Option<(Ty, u64)> {
    let plain = value.attrib() == values::nil() && !value.is_altrep();
    let scalar = plain && values::length(value) == 1;
    Some(match value.kind() {
        SexpType::Integer if scalar => (Ty::Int, values::ints(value)[0] as u32 as u64),
        SexpType::Logical if scalar => (Ty::Lgl, values::ints(value)[0] as u32 as u64),
        SexpType::Real if scalar => (Ty::Dbl, values::reals(value)[0].to_bits()),
        SexpType::Integer if plain => (Ty::IntVec, value.as_ptr() as u64),
        SexpType::Logical if plain => (Ty::LglVec, value.as_ptr() as u64),
        SexpType::Real if plain => (Ty::DblVec, value.as_ptr() as u64),
        SexpType::Record if value.attrib() == values::nil() && !value.is_object() => {
            (Ty::Rec(value.record_layout()), value.as_ptr() as u64)
        }
        _ => return None,
    })
}

/// Calls the native function at `address` with the call budget and arguments as 64-bit words,
/// storing its value's bits in `out` and returning its status.
type Trampoline = unsafe extern "C" fn(usize, i64, *const u64, *mut u64) -> i64;

/// The trampoline for native functions of this machine signature, compiled on first use.
unsafe fn trampoline(params: &[Ty], result: Ty) -> Option<Trampoline> {
    thread_local! {
        static TRAMPOLINES: RefCell<FxHashMap<SmallVec<[Type; 6]>, Trampoline>> =
            RefCell::default();
    }
    let mut key: SmallVec<[Type; 6]> = params.iter().map(|&ty| machine_type(ty)).collect();
    key.push(machine_type(result));
    if let Some(found) = TRAMPOLINES.with_borrow(|trampolines| trampolines.get(&key).copied()) {
        return Some(found);
    }
    let compiled = codegen::with_module(|module| {
        let call_conv = module.isa().default_call_conv();
        let mut outer = Signature::new(call_conv);
        outer.params.extend([AbiParam::new(I64); 4]);
        outer.returns.push(AbiParam::new(I64));
        let id = module
            .declare_anonymous_function(&outer)
            .map_err(|_| Refusal("Cranelift rejected the trampoline declaration"))?;
        let mut context = module.make_context();
        context.func.signature = outer;
        let mut function_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut function_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let &[address, budget, words, out] = builder.block_params(entry) else {
                return Err(Refusal("a trampoline without its parameters"));
            };
            let mut arguments: SmallVec<[Value; 5]> = SmallVec::new();
            arguments.push(budget);
            for (index, &ty) in params.iter().enumerate() {
                arguments.push(builder.ins().load(
                    machine_type(ty),
                    MemFlagsData::trusted(),
                    words,
                    8 * index as i32,
                ));
            }
            let inner = builder.import_signature(signature(params, result, call_conv));
            let call = builder.ins().call_indirect(inner, address, &arguments);
            let &[status, value] = builder.inst_results(call) else {
                return Err(Refusal("a native call without a status and a value"));
            };
            builder.ins().store(MemFlagsData::trusted(), value, out, 0);
            builder.ins().return_(&[status]);
            builder.seal_all_blocks();
            builder.finalize(module.isa().frontend_config());
        }
        module
            .define_function(id, &mut context)
            .map_err(|_| Refusal("Cranelift rejected the trampoline"))?;
        module.clear_context(&mut context);
        module
            .finalize_definitions()
            .map_err(|_| Refusal("Cranelift could not finalize the trampoline"))?;
        Ok(module.get_finalized_function(id) as usize)
    });
    let address = match compiled {
        Ok(address) => address,
        Err(Refusal(why)) => {
            if crate::logging() {
                eprintln!("jit: no trampoline for {params:?} -> {result:?}: {why}");
            }
            return None;
        }
    };
    // SAFETY: the code just compiled has exactly this signature, in the C calling convention.
    let trampoline = std::mem::transmute::<usize, Trampoline>(address);
    TRAMPOLINES.with_borrow_mut(|trampolines| trampolines.insert(key, trampoline));
    Some(trampoline)
}
