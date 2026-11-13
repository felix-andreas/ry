//! ry's typed core: native values (`values`) and the compiler that turns a strict typed closure
//! into machine code over them.
//!
//! A call of a typed closure from R reaches [`call`], which reads the argument values' types,
//! compiles the closure for them once (lowering its body to the IR, inserting the reference
//! counts and generating code), converts the arguments to native values, runs the code and
//! converts its value back. Whatever the compiler cannot handle, and every call whose code gives
//! up partway, runs in the interpreter instead, which the caller does when [`call`] returns
//! `None`. Code gives up before anything it did is visible, so running the call again from the
//! start is correct, with one exception: code that called an R function ([`ry_call_r`]) may have
//! done something visible, so once it has, giving up is an error instead of a second run.

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

pub mod backend;
pub mod fusion;
pub mod ir;
pub mod lower;
pub mod ownership;
pub mod shapes;
pub mod unbox;
pub mod values;

use {
    crate::{
        backend::{Callee, Code, Trampoline},
        ir::{Item, Type},
        lower::{Callees, Lowered, RCall, RResult, Speculation},
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
        annotations::decline, builtins::Arg, bytecode::TypedCall, records::NativeValues,
        unwind::Unwind, values as rvalues,
    },
    rustc_hash::{FxHashMap, FxHashSet},
    smallvec::SmallVec,
    std::{
        borrow::Cow,
        cell::{Cell, RefCell},
        mem::MaybeUninit,
        time::{Duration, Instant},
    },
};

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
}

type Key = (Sexp, SmallVec<[Type; 4]>);

thread_local! {
    /// Compiled closures and refusals, by closure and argument types.
    static COMPILED: RefCell<FxHashMap<Key, Result<&'static Compiled, Refusal>>> =
        RefCell::new(FxHashMap::default());
    /// The closures being compiled, so mutual recursion stops instead of looping.
    static COMPILING: RefCell<FxHashSet<Key>> = RefCell::new(FxHashSet::default());
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
    /// The error an R call from compiled code raised, which [`call`] raises once the code has
    /// given up.
    static PARKED: RefCell<Option<Unwind>> = const { RefCell::new(None) };
    /// The interpreter's call whose compiled code is running, which an R call the code makes
    /// runs in a frame of. A typed callee that compiled code calls directly has no call of its
    /// own, so its R calls see its caller's.
    static RUNNING: Cell<Option<Running>> = const { Cell::new(None) };
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

/// How many closures a session compiles at most, since each keeps its closure alive.
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
/// `lazy` says that an argument is a promise read ahead without forcing it whose value depends on
/// variables, which code that calls R could change before the interpreter would have forced it.
pub unsafe fn call(
    typed: &TypedCall<'_>,
    values: &[Sexp],
    lazy: bool,
) -> Option<Result<Sexp, Unwind>> {
    let function = typed.function;
    if !interp::annotations::is_typed(function) {
        return None;
    }
    if function.has_gp(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP | gp::CLOSURE_TRACE) {
        decline(|| String::from("it is being debugged or traced"));
        return None;
    }
    let mut params: SmallVec<[Type; 4]> = SmallVec::new();
    for (position, &value) in values.iter().enumerate() {
        // An `I64` formal converts a whole R number at the boundary, which `Arguments::push`
        // does, or gives up for the interpreter to raise the error.
        if interp::annotations::declares_i64(function, position)
            && matches!(type_of(value), Some(Type::Int | Type::Dbl | Type::I64))
        {
            params.push(Type::I64);
            continue;
        }
        let Some(ty) = type_of(value) else {
            decline(|| format!("argument {}: {}", position + 1, boundary_refusal(value)));
            return None;
        };
        // A length-one vector passed where the closure declares a vector is that vector, not a
        // scalar.
        let ty = match ty.item() {
            Some(item @ (Item::Int | Item::Dbl | Item::Lgl | Item::Str | Item::I64))
                if interp::annotations::declares_vector(function, position) =>
            {
                Type::vector(item)
            }
            _ => ty,
        };
        params.push(ty);
    }
    // Converting a tag confirms its shapes, which the code compiled next must consider.
    let mut arguments = Arguments::default();
    for (position, (&value, &ty)) in values.iter().zip(&params).enumerate() {
        if let Err(reason) = arguments.push(value, ty) {
            arguments.release();
            decline(|| format!("argument {} does not convert: {reason}", position + 1));
            return None;
        }
    }
    let Some(compiled) = compiled_for(function, &params) else {
        arguments.release();
        return None;
    };
    if lazy && compiled.calls_r {
        arguments.release();
        decline(|| {
            String::from(
                "it calls R, which could change a variable an argument reads before R would force \
                 that argument: compute the argument into a variable before the call",
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
    let heap = interp::heap();
    let mark = heap.handle_mark();
    let mut out = [0u64; 2];
    let trampoline: Trampoline = std::mem::transmute(compiled.code.trampoline);
    let r_calls = R_CALLS.get();
    let outer = RUNNING.replace(Some(Running {
        call: typed.call,
        function,
        args: typed.args.as_ptr(),
        count: typed.args.len(),
        sys_parent: typed.sys_parent,
    }));
    let status = trampoline(depth_budget(), arguments.words.as_ptr(), out.as_mut_ptr());
    RUNNING.set(outer);
    if status != 0 {
        arguments.release();
        heap.release_handles(mark);
        if let Some(unwind) = PARKED.take() {
            return Some(Err(unwind));
        }
        if R_CALLS.get() != r_calls {
            return Some(Err(interp::unwind::error(
                "a typed function stopped at an operation only the interpreter finishes after it \
                 had called R, so it cannot run again in the interpreter: the operation is one \
                 that raises an error or a warning, or a value that leaves the range its type \
                 holds",
            )));
        }
        decline(|| {
            String::from(
                "its native code stopped at an operation only the interpreter finishes, such as \
                 one that raises an error or a warning, and the interpreter ran the call again",
            )
        });
        return None;
    }
    let value = to_r(compiled.result, out);
    drop_words(compiled.result, out);
    arguments.release();
    heap.release_handles(mark);
    interp::interp().visible = true;
    Some(value)
}

/// The compiled code for calling `function` with arguments of types `params`, compiling it on
/// first use. `None` when the closure does not compile for them.
pub unsafe fn compiled_for(function: Sexp, params: &[Type]) -> Option<&'static Compiled> {
    compiled_or_refusal(function, params).ok()
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
        Some(Some(compiled))
            if !still_resolves(compiled)
                || compiled.epoch.is_some_and(|epoch| epoch != shapes::epoch()) => {}
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
    if COMPILED.with_borrow(|compiled| compiled.values().filter(|entry| entry.is_ok()).count())
        >= MAX_COMPILED
    {
        decline(|| format!("the session compiled its limit of {MAX_COMPILED} typed functions"));
        return Err(Refusal::new(
            "the session compiled its limit of typed functions",
        ));
    }
    if !COMPILING.with_borrow_mut(|compiling| compiling.insert(key.clone())) {
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
            Ok(&*Box::leak(Box::new(compiled)))
        }
        Err(refusal) => {
            if started.is_some() {
                eprintln!("typed: refused {params:?} after {} us", own.as_micros());
            }
            decline(|| format!("no code for {params:?}: {}", refusal.0));
            Err(refusal)
        }
    };
    let found = compiled.clone();
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
    COMPILED.with_borrow_mut(|table| {
        if compiled.is_err() && table.len() >= MAX_REFUSALS {
            table.retain(|_, entry| entry.is_ok());
            AWAITING.with_borrow_mut(|awaiting| awaiting.clear());
        }
        table.insert(key, compiled);
    });
    found
}

/// Whether `function` has compiled code for some argument types.
pub fn has_compiled(function: Sexp) -> bool {
    COMPILED.with_borrow(|compiled| {
        compiled
            .iter()
            .any(|((closure, _), entry)| *closure == function && entry.is_ok())
    })
}

/// Whether every function lookup `compiled` assumes still finds the same function.
unsafe fn still_resolves(compiled: &Compiled) -> bool {
    compiled.speculations.iter().all(|assumed| {
        let resolve = if interp::values::is_function(assumed.function) {
            lower::resolve_function
        } else {
            lower::resolve_value
        };
        resolve(assumed.symbol, assumed.env)
            .is_some_and(|now| now.cell == assumed.cell && now.function == assumed.function)
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
        let arguments: SmallVec<[Option<(SexpType, bool)>; 4]> =
            params.iter().map(|&ty| storage(ty)).collect();
        let result = storage(lowered.function.result);
        if !interp::annotations::admits_storage(function, &arguments, result) {
            return Err(Refusal::new("the types fail the closure's annotations"));
        }
        for &shape in &lowered.constructed {
            shapes::confirm(shape);
        }
        if earliest(lowered.epoch, linker.epoch).is_none_or(|epoch| epoch == shapes::epoch()) {
            return generate(params, lowered, linker);
        }
    }
    Err(Refusal::new("the shapes a `match` tests for keep growing"))
}

/// How many times [`compile`] lowers before the shapes settle.
const MAX_SHAPE_ROUNDS: usize = 4;

/// Lowers `function` with a result type that its recursive calls agree with.
unsafe fn lower_settled(function: Sexp, params: &[Type]) -> Result<(Lowered, Linker), Refusal> {
    let declared = declared_result(function);
    let guesses: SmallVec<[Type; 5]> = declared.into_iter().chain(GUESSES).collect();
    let mut refusal = Refusal::new("the result type depends on the recursion");
    for &first in &guesses {
        let mut guess = first;
        for _ in 0..=MAX_REVISIONS {
            let mut linker = Linker::default();
            let lowered = match lower::lower(function, params, guess, declared, &mut linker) {
                Ok(lowered) => lowered,
                Err(why) => {
                    refusal = why;
                    break;
                }
            };
            let result = lowered.function.result;
            if result == guess || !calls_itself(&lowered.function) {
                return Ok((lowered, linker));
            }
            guess = result;
        }
    }
    Err(refusal)
}

/// Machine code for a lowered function whose shapes are confirmed.
unsafe fn generate(params: &[Type], lowered: Lowered, linker: Linker) -> Result<Compiled, Refusal> {
    let mut ir = lowered.function;
    unbox::unbox(&mut ir);
    fusion::fuse(&mut ir);
    ownership::insert(&mut ir);
    let sites: Vec<usize> = lowered
        .r_calls
        .iter()
        .map(|site| site as *const RCall as usize)
        .collect();
    let code = backend::compile(&ir, &linker.callees, &sites)?;
    let calls_r = linker.calls_r
        || lowered
            .r_calls
            .iter()
            .any(|site| !site.diverges && !site.effect_free);
    // The code calls each function and conversion it was compiled against, and keeps every site
    // for as long as it lives, which is the whole session.
    let heap = interp::heap();
    for site in &lowered.r_calls {
        heap.add_global_root(site.function);
        heap.add_global_root(site.call);
        for conversion in site
            .arguments
            .iter()
            .filter_map(|argument| argument.conversion)
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
    let epoch = earliest(lowered.epoch, linker.epoch);
    Ok(Compiled {
        code,
        params: SmallVec::from_slice(params),
        result: ir.result,
        speculations,
        epoch,
        calls_r,
    })
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
    speculations: Vec<Speculation>,
    epoch: Option<u64>,
    calls_r: bool,
}

impl Callees for Linker {
    unsafe fn callee(&mut self, function: Sexp, params: &[Type]) -> Result<(usize, Type), Refusal> {
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
        self.callees.push(Callee {
            address: compiled.code.address,
            params: SmallVec::from_slice(params),
            result: compiled.result,
        });
        Ok((self.callees.len() - 1, compiled.result))
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
fn storage(ty: Type) -> Option<(SexpType, bool)> {
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
        Type::Set(_) | Type::Dict { .. } => return None,
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
/// strings, numbers or logicals of one storage type a list of them, a plain tag any tag, and a
/// value of the table API a table.
/// A record or tuple has none, because a typed closure takes one only from typed code.
unsafe fn type_of(value: Sexp) -> Option<Type> {
    if interp::dataframe::is_table_value(value) {
        return Some(Type::Table);
    }
    if value.is_altrep() || value.attrib() != rvalues::nil() {
        return None;
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
        _ => return None,
    };
    if length == 1 && !array {
        return Some(Type::of_item(item));
    }
    Some(Type::Vector { item, array })
}

/// Why an argument from R has no type in the typed core, and what to pass instead.
unsafe fn boundary_refusal(value: Sexp) -> String {
    if value.kind() == SexpType::Record {
        return String::from(
            "it is a record or tuple from R, which typed code takes only from typed code: pass \
             its fields as parameters, or build it inside a typed function",
        );
    }
    if value.attrib() != rvalues::nil() {
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
            Type::Bool | Type::Null | Type::Set(_) | Type::Dict { .. } | Type::Record(_) => {
                return Err(UNTYPED);
            }
        }
        Ok(())
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

/// A string list holding the strings of the character vector `value`.
unsafe fn string_list(value: Sexp) -> *mut List {
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
    if depth % STACK_CHECK_LEVELS == 0 {
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
    if field.is_array() && !field.is_altrep() && field.attrib() == rvalues::nil() {
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
            if !(0..length).all(|index| usable_string(array.element(index))) {
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
    // A lazy record keeps its whole native tree alive while the R heap sees one small record,
    // so trees R drops would never make the collector run. Native objects past a limit run a
    // minor collection, and the limit doubles past what survives, so values typed code holds
    // itself cost collections only geometrically often.
    if values::ry_live_objects() > NATIVE_COLLECTION_AT.get() {
        interp::heap().collect(Collection::Minor);
        NATIVE_COLLECTION_AT.set((values::ry_live_objects() * 2).max(NATIVE_COLLECTION_FLOOR));
    }
    values::ry_list_dup(boxed as *mut List);
    interp::records::alloc_lazy(layout, fields.len(), boxed, NATIVE_VALUES)
        .inspect_err(|_| values::ry_list_drop(boxed as *mut List))
}

/// The fewest live native objects at which [`from_box`] runs a collection.
const NATIVE_COLLECTION_FLOOR: i64 = 1 << 20;

/// How R fills the lazy records [`from_box`] makes and releases their boxes.
const NATIVE_VALUES: NativeValues = NativeValues {
    settle,
    release: release_box,
};

/// Fills the lazy record `record` from its box. Scalars and strings become slots, and a record,
/// tuple or tag field becomes a lazy record of its own, so reading one field of a deep tree
/// converts one level.
unsafe fn settle(record: Sexp) -> Result<(), Unwind> {
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
/// words of its converted value to `out`. 0 when the call returned, 1 when it raised an error,
/// which [`call`] raises once the code has given up.
///
/// # Safety
///
/// `site` is a site the code was compiled with, `words` holds the words of its arguments, and
/// `out` has room for the words of its result.
pub unsafe extern "C" fn ry_call_r(site: *const RCall, words: *const u64, out: *mut u64) -> i32 {
    // An effect-free call changes nothing a second run would see unless it warns, so only then
    // does it count as a call the code cannot make again.
    let effect_free = (*site).effect_free;
    let warnings = interp::conditions::warnings_raised();
    if !effect_free {
        R_CALLS.set(R_CALLS.get() + 1);
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
            interp::eval::run_in_frame(&typed, |env| call_r(&*site, words, out, env))
        }
        None => call_r(&*site, words, out, (*site).env),
    };
    heap.release_handles(mark);
    if effect_free && interp::conditions::warnings_raised() != warnings {
        R_CALLS.set(R_CALLS.get() + 1);
    }
    // Compiled code holds a table as a bare word, which the collector does not see, so the table
    // stays rooted until the call that entered typed code releases its handles.
    if matches!((*site).result, RResult::Table)
        && outcome.is_ok()
        && let Some(value) = Sexp::from_raw(*out as *mut _)
    {
        heap.root(value);
    }
    match outcome {
        Ok(()) => 0,
        Err(unwind) => {
            PARKED.set(Some(unwind));
            1
        }
    }
}

unsafe fn call_r(
    site: &RCall,
    mut words: *const u64,
    out: *mut u64,
    env: Sexp,
) -> Result<(), Unwind> {
    let heap = interp::heap();
    let mut args: SmallVec<[Arg; 4]> = SmallVec::new();
    for argument in &site.arguments {
        let mut native = [0u64; 2];
        for word in native.iter_mut().take(backend::words(argument.ty).len()) {
            *word = *words;
            words = words.add(1);
        }
        let mut value = to_r(argument.ty, native)?;
        heap.root(value);
        if let Some(conversion) = argument.conversion {
            value = convert(conversion, conversion.call.cdr().car(), value, site.env)?;
            heap.root(value);
        }
        let promise = interp::eval::forced_promise(argument.code, value)?;
        heap.root(promise);
        args.push(Arg {
            tag: argument.tag,
            value: promise,
        });
    }
    let value = interp::eval::apply(site.call, site.function, &args, env)?;
    let (conversion, ty) = match site.result {
        RResult::Unused => return Ok(()),
        RResult::Converted(conversion, ty) => (conversion, ty),
        RResult::Table => {
            if !interp::dataframe::is_table_value(value) {
                return Err(interp::unwind::error(format!(
                    "`{}` gave a value of type `{}` where the typed function was compiled for a \
                     table value",
                    interp::deparse::deparse_first_line(site.call.car()),
                    value.kind().name()
                )));
            }
            *out = value.as_ptr() as u64;
            return Ok(());
        }
    };
    heap.root(value);
    let converted = convert(conversion, site.call, value, site.env)?;
    heap.root(converted);
    write_converted(conversion, ty, converted, out)
}

/// Applies the conversion `conversion` to `value`, the value of the code `code`, as the
/// interpreter applies it.
unsafe fn convert(
    conversion: lower::Conversion,
    code: Sexp,
    value: Sexp,
    env: Sexp,
) -> Result<Sexp, Unwind> {
    let promise = interp::eval::forced_promise(code, value)?;
    interp::heap().root(promise);
    let argument = Arg {
        tag: rvalues::nil(),
        value: promise,
    };
    interp::eval::apply(conversion.call, conversion.function, &[argument], env)
}

/// Writes the words of `value`, which the conversion `conversion` gave, as a native value of
/// `ty`, the type it gave when the code compiled.
unsafe fn write_converted(
    conversion: lower::Conversion,
    ty: Type,
    value: Sexp,
    out: *mut u64,
) -> Result<(), Unwind> {
    let expected = storage(ty).map(|(kind, _)| kind);
    if expected != Some(value.kind())
        || rvalues::length(value) != 1 && !matches!(ty, Type::Vector { .. })
    {
        return Err(interp::unwind::error(format!(
            "`{}` gave a value of type `{}`, and the typed function was compiled for the type it \
             gave before, {ty:?}: convert the value to one type in R first",
            interp::deparse::deparse_first_line(conversion.call),
            value.kind().name()
        )));
    }
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
                "a conversion of what R returns gave a type typed code does not take from R",
            ));
        }
    }
    Ok(())
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
