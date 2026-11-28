//! Lowering a typed closure's body to the typed IR.
//!
//! Every expression gets the type R would compute from the argument types the function is compiled
//! for: `i %% 5L` of two integers is an integer, `seq_len(n)` a vector of integers, `paste0("a",
//! i)` a string. A local keeps one type for the whole body, and an expression of undetermined type
//! or a call the lowering does not know refuses the function, which then runs in the interpreter.
//!
//! Base and standard library calls become IR operations and calls of other typed closures direct
//! calls of their code. A local function or a literal passed to `lapply` or `vapply` is inlined,
//! reading the enclosing function's locals in place, which is what its environment would give it in
//! R since it never escapes.

use {
    crate::{
        Refusal,
        ir::{
            Arith, BitCount, Bits, Block, BlockId, Builder, Compare, Constant, Function, Inst,
            Item, Local, Math, Op, Overflow, Reduce, StrTest, Terminator, Text, Type,
        },
        shapes, values,
    },
    heap::{
        handle::Gc,
        object::{NA_INTEGER, Sexp, SexpType},
        records::{Fields, Layout},
    },
    interp::{
        eval::in_tail_position, numbers::Observed, records::rest_name,
        standard_library::Encoding, values as rvalues,
    },
    rustc_hash::FxHashMap,
    smallvec::SmallVec,
    std::{
        borrow::Cow,
        cell::{Cell, RefCell},
    },
    syntax::native_values::{reads_its_frame, uses_its_frame},
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
    pub function: Function,
    pub speculations: Vec<Speculation>,
    /// The R calls the code makes, as [`Op::CallR`] indexes them.
    pub r_calls: Vec<RCall>,
    /// What the code's matches over any tag relied on, when it has one.
    pub any_tag: Option<AnyTag>,
    /// The tag shapes the code constructs, which the caller confirms before it generates code.
    pub constructed: SmallVec<[u32; 4]>,
    /// Where the body reads each parameter, in the blocks of `function` ([`reads_before`]).
    pub param_reads: Vec<ParamRead>,
    /// The types of the defaults of the formals the call leaves out, in the formals' order: those
    /// past the parameters and those a parameter of type [`Type::Absent`] stands for. The code
    /// computes them itself ([`Lowering::left_out_defaults`]).
    pub defaults: SmallVec<[Type; 2]>,
    /// Whether R returns the value invisibly, as after an assignment or a loop
    /// ([`returns_invisibly`]).
    pub invisible: bool,
}

/// A read of parameter `param` in block `block`, which held `position` instructions when
/// lowering reached it, so the read comes before the instruction at that position. A read among
/// the arguments of an R call names that call's [`Op::CallR`] site in `into`. A read among the
/// arguments of a local function counts where the inlined body reads the formal, as R forces
/// the argument there ([`Lowering::param_aliases`]).
#[derive(Clone, Copy, Debug)]
pub struct ParamRead {
    pub block: BlockId,
    pub position: u32,
    pub param: u32,
    pub into: Option<usize>,
}

/// Which parameters, as bits by position among the first 64, the body has read where it may run
/// R code. R forces an argument where the body first reads it, so an R call the body makes
/// before that read could change what the argument computes, while the native entry has read it
/// ahead.
#[derive(Debug, Default)]
pub struct ReadsBefore {
    /// The parameters every path reads before the first instruction that may run R code.
    pub first: u64,
    /// The parameters that some path reads, and some path does not, before an instruction that may
    /// run R code, so whether R has forced the argument there depends on the path.
    pub ambiguous: u64,
    /// The parameters every path reads before each [`Op::CallR`], by site.
    pub sites: Vec<(usize, u64)>,
    /// The parameters the code passes among the arguments of an R call that may run R code on
    /// some path without having read them first, so R would force them inside that call.
    pub passed_late: u64,
    /// The parameters some path reads before it returns, outside the arguments of R calls.
    pub returned: u64,
}

/// [`ReadsBefore`] for `lowered`, where `runs_r` says which instructions may run R code. A read
/// among the arguments of a site that runs R counts at no point, since R forces it somewhere
/// inside the call.
pub fn reads_before(lowered: &Lowered, runs_r: impl Fn(&Op) -> bool) -> ReadsBefore {
    let blocks = &lowered.function.blocks;
    let mut acting = vec![false; lowered.r_calls.len()];
    for inst in blocks.iter().flat_map(|block| &block.insts) {
        if let Op::CallR(site, _) = inst.op
            && let Some(slot) = acting.get_mut(site)
        {
            *slot = runs_r(&inst.op);
        }
    }
    let mut passed = vec![0u64; lowered.r_calls.len()];
    // Each block's reads by position.
    let mut reads: Vec<SmallVec<[(u32, u64); 2]>> = vec![SmallVec::new(); blocks.len()];
    for read in &lowered.param_reads {
        let bit = if read.param < 64 { 1 << read.param } else { 0 };
        if let Some(site) = read.into
            && acting.get(site).is_none_or(|&acting| acting)
        {
            match passed.get_mut(site) {
                Some(slot) => *slot |= bit,
                None => return ReadsBefore::unknown(),
            }
        } else if let Some(list) = reads.get_mut(read.block.0 as usize) {
            list.push((read.position, bit));
        }
    }
    for list in &mut reads {
        list.sort_unstable_by_key(|&(position, _)| position);
    }
    // What every path into a block has read, and what some path has.
    let every = entering(blocks, &reads, u64::MAX, |a, b| a & b);
    let some = entering(blocks, &reads, 0, |a, b| a | b);
    let mut before = ReadsBefore {
        first: u64::MAX,
        ..ReadsBefore::default()
    };
    for (index, block) in blocks.iter().enumerate() {
        let (mut all, mut any) = (every[index], some[index]);
        let mut events = reads[index].iter().peekable();
        for (position, inst) in block.insts.iter().enumerate() {
            while let Some(&&(at, bit)) = events.peek()
                && at as usize <= position
            {
                all |= bit;
                any |= bit;
                events.next();
            }
            if let Op::CallR(site, _) = inst.op {
                match before.sites.iter_mut().find(|(known, _)| *known == site) {
                    Some((_, read)) => *read &= all,
                    None => before.sites.push((site, all)),
                }
                before.passed_late |= passed.get(site).copied().unwrap_or(u64::MAX) & !all;
            }
            if runs_r(&inst.op) {
                before.first &= all;
                before.ambiguous |= any & !all;
            }
        }
        for &(at, bit) in events {
            debug_assert!(at as usize >= block.insts.len());
            any |= bit;
        }
        if let Terminator::Return(_) = block.terminator {
            before.returned |= any;
        }
    }
    before
}

impl ReadsBefore {
    /// What a lowering whose reads name sites it does not have reads: nothing settles, every
    /// parameter is late and ambiguous, and every one counts as returned.
    fn unknown() -> ReadsBefore {
        ReadsBefore {
            first: 0,
            ambiguous: u64::MAX,
            sites: Vec::new(),
            passed_late: u64::MAX,
            returned: u64::MAX,
        }
    }
}

/// What the paths into each block of `blocks` have read, joined by `join` from `start`, the value
/// a block no path has reached yet holds. The entry starts with nothing read.
fn entering(
    blocks: &[Block],
    reads: &[SmallVec<[(u32, u64); 2]>],
    start: u64,
    join: impl Fn(u64, u64) -> u64,
) -> Vec<u64> {
    let mut entering = vec![start; blocks.len()];
    if let Some(entry) = entering.first_mut() {
        *entry = 0;
    }
    loop {
        let mut changed = false;
        for (index, block) in blocks.iter().enumerate() {
            let read = reads[index]
                .iter()
                .fold(entering[index], |bits, &(_, bit)| bits | bit);
            for successor in block.terminator.successors() {
                if let Some(slot) = entering.get_mut(successor.0 as usize)
                    && join(*slot, read) != *slot
                {
                    *slot = join(*slot, read);
                    changed = true;
                }
            }
        }
        if !changed {
            return entering;
        }
    }
}

/// What the matches over any tag in one lowering relied on. Each tests for the confirmed shapes
/// whose layout or tag one of its patterns names, so code written for one process runs in another
/// as that process would compile it exactly when every shape the other process has confirmed and
/// a pattern names is among `tested`.
#[derive(Clone, Debug, Default)]
pub struct AnyTag {
    /// The earliest [`shapes::epoch`] a match read the confirmed shapes at.
    pub epoch: u64,
    /// The layouts the patterns name exactly.
    pub layouts: SmallVec<[u32; 4]>,
    /// The tags the patterns name with any fields.
    pub tags: SmallVec<[u32; 4]>,
    /// The shapes the matches test for.
    pub tested: SmallVec<[u32; 4]>,
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
    pub result: RResult,
    /// Whether the call is `stop()`, which raises an error and never returns, so it cannot
    /// change a variable a lazy argument reads before R would force it.
    pub diverges: bool,
    /// Whether the call changes nothing a second run of it would see: a table function
    /// ([`gives_table_value`]) or a read of a table's column. Code that gives
    /// up after one may run again in the interpreter, no lazy argument can observe it, and it
    /// may stand inside another call's arguments.
    pub effect_free: bool,
    /// The parameters, as bits by position, that every path through the code reads before this
    /// call ([`reads_before`]), which the native entry settles before the call.
    pub read_before: u64,
}

/// What compiled code does with the value of an [`RCall`].
#[derive(Clone, Copy)]
pub enum RResult {
    /// The call is a statement, whose value is unused.
    Unused,
    /// The value goes through this `Num.from_r`, `Str.from_r`, `Bool.from_r`, `List.from_r`,
    /// `List.from_r_or` or `List.from_r_missing` call, which gives this type.
    Converted(Conversion, Type),
    /// The call is one of the table API's that gives a table value
    /// ([`gives_table_value`]), which the code holds as it is.
    Table,
    /// The site reads the global variable its `call` names, which must hold a value of this type
    /// each time the code runs. It calls nothing.
    Global(Type),
}

/// An argument of an [`RCall`], written as `code`.
pub struct RArgument {
    pub tag: Sexp,
    pub code: Sexp,
    pub passed: Passed,
}

/// What compiled code passes for an [`RArgument`].
#[derive(Clone, Copy)]
pub enum Passed {
    /// A native value of this type, through the conversion when the code converts it with
    /// `Num.to_r` or `List.to_r`.
    Value(Type, Option<Conversion>),
    /// The code itself, which R evaluates in the typed closure's environment when the function
    /// asks for it: a literal, which may be one with no ry type such as `NA`, or a variable from
    /// outside the function, whose binding R reads when the call runs.
    Code,
}

/// A standard library conversion between ry and R values, applied as the interpreter applies it,
/// so it raises the same errors.
#[derive(Clone, Copy)]
pub struct Conversion {
    pub call: Sexp,
    pub function: Sexp,
    /// The item type the checker gave a `List.from_r` or `List.from_r_or` (for
    /// `List.from_r_missing`, the type inside `:Ok`), or the number type it gave a `Num.from_r`,
    /// which the preflight writes around the call (`.ry_items(List.from_r(x), "F64")`). It types
    /// the conversion before the interpreter has run it.
    pub checked: Option<Item>,
    /// The type of a `List.from_r_or`'s default, which the code computes before the R call and
    /// passes after the call's arguments, so its words follow theirs.
    pub default: Option<Type>,
}

impl Conversion {
    /// Whether the code takes the type of the value this conversion gives from what the
    /// interpreter saw it give, which a later run may contradict: a `Num.from_r`, `List.from_r`,
    /// `List.from_r_or` or `List.from_r_missing` the checker did not type.
    pub unsafe fn guessed(self) -> bool {
        self.checked.is_none()
            && matches!(
                &*rvalues::symbol_name(self.call.car()),
                "Num.from_r" | "List.from_r" | "List.from_r_or" | "List.from_r_missing"
            )
    }
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
    /// A run of this `Num.from_r` or `List.from_r` call, or of one of its siblings.
    Conversion(Sexp),
    /// The loading of the namespace of this package name.
    Namespace(Sexp),
    /// The forcing of this lazy-load promise, which a namespace binds a function to until the
    /// first use reads it from the package database.
    Promise(Sexp),
    /// A binding of this symbol that a lookup from this environment finds.
    Binding(Sexp, Sexp),
}

/// Whether the interpreter has done what `awaited` waits for.
pub unsafe fn arrived(awaited: Awaited) -> bool {
    match awaited {
        Awaited::Conversion(call) => interp::numbers::observed_conversion(call).is_some(),
        Awaited::Namespace(package) => loaded_namespace(package).is_some(),
        Awaited::Promise(promise) => promise.car() != rvalues::unbound(),
        Awaited::Binding(symbol, env) => !matches!(global_binding(symbol, env), Global::Unbound),
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

/// Whether the definition `formals`/`body` may carry a signature, as
/// `interp::annotations::is_typed` tests a closure's.
unsafe fn typed_definition(formals: Sexp, body: Sexp) -> bool {
    let key = if formals.kind() == SexpType::Pairlist {
        formals
    } else {
        body
    };
    key.has_gp(heap::object::gp::TYPED_DEFINITION)
}

/// The signature slot of a closure's first formal, after the return value's and the rest
/// parameter's ([`interp::annotations::stream_signature`]).
const FIRST_SLOT: usize = 2;

/// The index [`Op::CallTyped`] uses for the function calling itself.
pub const SELF: usize = usize::MAX;

/// How deeply local functions inline into each other.
const MAX_INLINE_DEPTH: usize = 8;

/// Base R closures that force every argument they are passed before they run R code a caller
/// could have written: they format, print or report what they are given and call no function
/// it names. A ry value crosses to R without a class, so `print` and `format` reach base methods.
const FORCING_BASE_FUNCTIONS: [&str; 11] = [
    "cat",
    "format",
    "message",
    "paste",
    "paste0",
    "print",
    "sprintf",
    "stop",
    "toString",
    "warning",
    "writeLines",
];

/// How many variables and walks a function's lowering widens to more tags before it gives up.
const MAX_WIDENINGS: usize = 8;
/// How many typed functions a call of a typed function may sit inside and still inline. The
/// lambdas between them do not count, so a small function called in a lambda of a lambda
/// inlines and its loops can fuse; `MAX_INLINE_DEPTH` bounds all levels together.
const MAX_TYPED_INLINE_DEPTH: usize = 4;
/// The most calls and operators the body of a typed function may hold and still inline into
/// its callers.
const MAX_INLINE_CALLS: usize = 48;
/// The most calls of itself a function may hold and still lower into itself, and the most
/// copies of its body those calls may add. Each call is another copy to compile, so a function
/// that recurses from many places, as an evaluator of a tree with several node kinds does,
/// would multiply its compile time.
const MAX_SELF_CALLS: usize = 2;
/// How deeply expressions nest, which bounds the lowering's own recursion.
const MAX_NESTING: usize = 256;

/// Lowers `closure` for arguments of `params`, assuming a recursive call returns `guess`. A
/// refused lowering sets `returned` to the type of the `return`s it lowered before the refusal,
/// or else of a branch of an `if` whose value is the function's result, which is a better guess
/// when the guess itself was what the body could not use.
pub unsafe fn lower(
    closure: Sexp,
    params: &[Type],
    guess: Type,
    declared: Option<Type>,
    callees: &mut dyn Callees,
    returned: &mut Option<Type>,
) -> Result<Lowered, Refusal> {
    // A typed function whose body is refused where it would inline is called instead, so one
    // function the typed core cannot compile does not keep its callers off native code. The
    // builder already holds part of the refused body by then, so the caller is lowered again.
    let mut called: SmallVec<[Sexp; 2]> = SmallVec::new();
    // A variable takes the type of the first value assigned to it. A later tag of another shape,
    // or a list of such tags, widens the variable's tags to the union of both, and the function
    // is lowered again with every variable of that name holding the union from its first
    // assignment. A walk's state widens the same way, keyed by its step function.
    let mut widened: SmallVec<[(Sexp, u32); 2]> = SmallVec::new();
    loop {
        let mut refused = None;
        let mut widen = None;
        let result = lower_calling(
            closure,
            params,
            guess,
            declared,
            &mut *callees,
            (&called, &widened),
            (&mut refused, &mut widen),
            returned,
        );
        match (result, refused, widen) {
            (Err(_), Some(function), _) => called.push(function),
            (Err(refusal), None, Some((key, union))) => {
                let count = widened.len();
                match widened.iter_mut().find(|(held, _)| *held == key) {
                    Some((_, held)) if shapes::within(union, *held) => return Err(refusal),
                    Some((_, held)) => *held = shapes::join(*held, union),
                    None if count < MAX_WIDENINGS => widened.push((key, union)),
                    None => return Err(refusal),
                }
            }
            (result, ..) => return result,
        }
    }
}

/// Lowers `closure` with the typed functions in `called` called, never inlined, and sets
/// `refused` to the innermost typed function whose body was refused where it inlined.
#[allow(clippy::too_many_arguments)]
unsafe fn lower_calling(
    closure: Sexp,
    params: &[Type],
    guess: Type,
    declared: Option<Type>,
    callees: &mut dyn Callees,
    (called, widened): (&[Sexp], &[(Sexp, u32)]),
    (refused, widen): (&mut Option<Sexp>, &mut Option<(Sexp, u32)>),
    returned: &mut Option<Type>,
) -> Result<Lowered, Refusal> {
    let formals = rvalues::pairlist_items(closure.car());
    if formals.len() < params.len() {
        return Err(Refusal::new(
            "the call passes more arguments than the function takes",
        ));
    }
    let body = interp::bytecode::closure_body(closure);
    let invisible = returns_invisibly(closure).map_err(Refusal::new)?;
    let name = String::from("typed");
    let mut lowering = Lowering {
        builder: Builder::new(name, params),
        closure,
        env: closure.tag(),
        params: SmallVec::from_slice(params),
        guess,
        declared,
        tails: SmallVec::new(),
        body,
        callees,
        scopes: vec![Scope {
            names: FxHashMap::default(),
            id: 0,
            entry: (BlockId(0), 0),
        }],
        scope_ids: 1,
        variables: Vec::new(),
        loops: Vec::new(),
        inlined: Vec::new(),
        inlining: SmallVec::new(),
        self_inlines: 0,
        tail_chain: true,
        called,
        refused,
        widened,
        widen,
        speculations: Vec::new(),
        nesting: 0,
        control: 0,
        regions: Vec::new(),
        unusable: SmallVec::new(),
        any_tag: None,
        returned: None,
        tail_branch: None,
        constructed: SmallVec::new(),
        r_calls: Vec::new(),
        r_use: None,
        converted: None,
        checked_items: None,
        settled_empty: false,
        pending: Vec::new(),
        forward: None,
        declared_loops: Vec::new(),
        probing: 0,
        captures: Vec::new(),
        param_variables: SmallVec::new(),
        param_aliases: Vec::new(),
        formal_reads: SmallVec::new(),
        reassigned: Vec::new(),
        param_reads: Vec::new(),
    };
    tail_matches(body, &mut lowering.tails);
    let mut assigned = Vec::new();
    assigned_symbols(body, &mut assigned);
    for (index, &(symbol, _)) in formals.iter().enumerate() {
        if symbol == rvalues::symbols().dots {
            return Err(Refusal::new("a function with `...`"));
        }
        let param = Local(index as u32);
        match params.get(index) {
            None => {
                lowering.bind(symbol, Binding::Hidden);
                continue;
            }
            // The parameter is never read, but keeps the others at their formals' positions.
            Some(Type::Absent) => {
                lowering.param_variables.push(param);
                lowering.bind(symbol, Binding::Hidden);
                continue;
            }
            Some(_) => {}
        }
        let variable = if assigned.contains(&symbol) {
            // An assigned parameter gets an owned copy, since the parameter only borrows.
            let ty = params[index];
            lowering.builder.emit(ty, Op::Copy(param))
        } else {
            param
        };
        lowering.param_variables.push(variable);
        lowering.bind(symbol, Binding::Value(variable));
    }
    // A specialization binds the function parameters of the function it specializes to the
    // functions passed in their place, each literal seeing the values it reads as parameters.
    let bindings = specialized_bindings(closure)?.unwrap_or_default();
    let mut capture_ids: SmallVec<[Option<u32>; 2]> = SmallVec::new();
    for (at, passed, reads) in bindings {
        let (callable, capture) = match passed {
            PassedFunction::Typed(name, function) => (Callable::Typed(name, function), None),
            PassedFunction::Library(name) => (Callable::Library(name), None),
            PassedFunction::Literal(formals, body) => {
                let mut names = FxHashMap::default();
                for (name, formal) in reads {
                    let Some(binding @ Binding::Value(_)) = lowering.lookup(formal) else {
                        return Err(Refusal::new("a specialization called without a value"));
                    };
                    names.insert(name, binding);
                }
                lowering.captures.push(names);
                let id = lowering.captures.len() as u32 - 1;
                (Callable::Captured(formals, body, id), Some(id))
            }
        };
        capture_ids.push(capture);
        match at {
            SpecializedAt::Formal(symbol) => lowering.bind(symbol, Binding::Function(callable)),
            SpecializedAt::Read { owner, name } => {
                let names = capture_ids
                    .get(owner)
                    .copied()
                    .flatten()
                    .and_then(|id| lowering.captures.get_mut(id as usize))
                    .ok_or(Refusal::new(
                        "a specialization's function read by no literal",
                    ))?;
                names.insert(name, Binding::Function(callable));
            }
        }
    }
    let defaults = if params.len() < formals.len() || params.contains(&Type::Absent) {
        lowering.left_out_defaults(&formals, &assigned)?
    } else {
        SmallVec::new()
    };
    let value = match lowering.expression(body) {
        Ok(value) => value,
        Err(why) => {
            *returned = lowering.returned.or(lowering.tail_branch);
            return Err(why);
        }
    };
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
    lowering.maintain_flags();
    Ok(Lowered {
        function: lowering.builder.function,
        speculations: lowering.speculations,
        r_calls: lowering.r_calls,
        any_tag: lowering.any_tag,
        constructed: lowering.constructed,
        param_reads: lowering.param_reads,
        defaults,
        invisible,
    })
}

/// A function passed to another: a local function or a literal, inlined where it is called, a
/// typed closure and the name that names it, or a function of base R or the standard library.
#[derive(Clone, Copy, Debug)]
enum Callable {
    /// Its formals, its body and the index of the scope that defines it, whose names are the
    /// only ones the body sees when it is inlined.
    Inline(Sexp, Sexp, usize),
    /// A local function or a literal passed to an inlined function, with the index of the
    /// capture of the names it sees ([`Lowering::capture`]).
    Captured(Sexp, Sexp, u32),
    Typed(Sexp, Sexp),
    /// The symbol that names it, which lowers as a call of that name ([`eta`]).
    Library(Sexp),
}

/// A value a call passes, or a function it passes to a parameter that takes one.
#[derive(Clone, Copy, Debug)]
enum Argument {
    Value(Local),
    Function(Callable),
}

/// A function a call passes to a typed function that lowers as a call of that function's
/// specialization ([`specialization`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PassedFunction {
    /// A function literal or local function, by its formals and body.
    Literal(Sexp, Sexp),
    /// A typed closure and the name that names it.
    Typed(Sexp, Sexp),
    /// A function of base R or the standard library, by the symbol that names it.
    Library(Sexp),
}

/// Where a passed function stands: in place of a formal of the function a call passes it to, or
/// as a function an earlier passed literal reads by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PassedAt {
    Formal(usize),
    Read { owner: usize, name: Sexp },
}

/// The functions a call passes, each before the functions it reads, with where it stands and,
/// for a literal, the names of the values it reads from its caller's scopes, in the order the
/// literal reads them.
type PassedFunctions = SmallVec<[(PassedAt, PassedFunction, SmallVec<[Sexp; 2]>); 1]>;

/// A value a passed literal reads from its caller, and whether the name may be unset there.
#[derive(Clone, Copy, Debug)]
struct CapturedValue {
    local: Local,
    maybe: bool,
}

/// How many passed functions deep a literal may read another passed function.
const MAX_PASSED_DEPTH: usize = 8;

/// The functions a call passes ([`PassedFunctions`]), the values their literals read from the
/// caller, in the order of the literals' names, and the names they read that no scope binds.
#[derive(Default)]
struct Passing {
    passed: PassedFunctions,
    captured: SmallVec<[CapturedValue; 4]>,
    globals: SmallVec<[Sexp; 2]>,
}

/// What a name in scope stands for.
#[derive(Clone, Copy, Debug)]
enum Binding {
    Value(Local),
    /// A name set on some paths to this point only, as after an `if` that sets it in one branch
    /// or after a loop whose body sets it. The local is one of the name's [`Variable`]s, and a
    /// read tests its flag first and gives the call up to the interpreter where the name is unset.
    Maybe(Local),
    /// A local function, or a function a call passed to an inlined function's parameter.
    Function(Callable),
    /// A local function that a branch or loop body defined, after it. The name may or may not be
    /// that function there, so any use refuses rather than reach a global of the same name.
    Hidden,
}

/// The names a branch or loop body created, each with its local and whether it is set on some
/// paths only ([`Binding::Maybe`]).
type Created = SmallVec<[(Sexp, Local, bool); 4]>;

/// How a name that branches set joins after them ([`Lowering::name_join`]).
enum NameJoin {
    /// Every end left it in one variable.
    Shared(Binding),
    /// The ends left it in variables of types that join into this one.
    Copied(Type),
}

#[derive(Clone)]
struct Scope {
    names: FxHashMap<Sexp, Binding>,
    /// Numbers the scopes of one lowering, so the variables of two inlined calls of one function
    /// stay apart.
    id: u32,
    /// Where the scope's code starts, a block and an instruction position in it, which clears the
    /// flags of its variables on every entry.
    entry: (BlockId, usize),
}

/// A local that holds a name's value in one scope. A name has one per type it takes, and the
/// lowering reuses it whenever the name is assigned a value of that type again, so every branch
/// of an `if` that sets the name writes the same local and the join needs no copies.
#[derive(Clone, Copy)]
struct Variable {
    scope: u32,
    /// The entry of the scope ([`Scope::entry`]).
    entry: (BlockId, usize),
    symbol: Sexp,
    local: Local,
    /// Whether the local holds the name's current value: false where the scope starts, set by
    /// every write of the local and cleared by a write of the name's other variables. Only a
    /// variable that a [`Binding::Maybe`] reads has one ([`Lowering::flag`]).
    flag: Option<Local>,
}

/// An inlined function's exit: the block a `return` jumps to, and the blocks that jumped there
/// early with their values, which join with the body's value once it is lowered.
#[derive(Clone)]
struct Inlined {
    exit: BlockId,
    returns_i64: bool,
    early: SmallVec<[(BlockId, Local); 2]>,
    /// Whether the function is annotated, which makes its calls of annotated functions strict
    /// ([`Lowering::attribute_reads`]).
    typed: bool,
}

/// The parameter reads computing one argument of a local function made: those of
/// [`Lowering::param_reads`] from `start` to `end`, lowered from block `entry` to block `exit`
/// through the blocks from `first_block` on.
#[derive(Clone, Copy)]
struct ArgumentReads {
    start: usize,
    end: usize,
    entry: BlockId,
    exit: BlockId,
    first_block: u32,
    /// Whether R computes the argument where the callee reads it even under a strict call,
    /// which holds for a default.
    lazy: bool,
    /// The R call sites of [`Lowering::r_calls`] the argument made, from `first_r_call` on.
    first_r_call: usize,
    end_r_call: usize,
}

/// Where an argument's paths go: [`Lowering::argument_paths`].
struct ArgumentPaths {
    reaches_exit: bool,
    /// Some path leaves the argument other than through its exit: a `return`, or a `break` or
    /// `next` of a loop outside it.
    escapes: bool,
}

const NO_ARGUMENT_READS: ArgumentReads = ArgumentReads {
    start: 0,
    end: 0,
    entry: BlockId(0),
    exit: BlockId(0),
    first_block: 0,
    lazy: true,
    first_r_call: 0,
    end_r_call: 0,
};

/// What a `match`'s value must be, which decides how [`Lowering::switch_shapes`] treats arms
/// whose types disagree.
#[derive(Clone, Copy)]
enum Expected {
    /// The program drops the value of this `match` call, as a statement's, so its arms may
    /// disagree. A name they set that code outside the call reads must still join after them.
    Dropped(Sexp),
    /// Nothing says what type the value has.
    Free,
    /// The function's declared result: a number converts toward it as the function's exit does.
    Declared(Type),
    /// The type the arms of every shape but a stray one give: the arm of a confirmed shape that
    /// gives another type leaves that shape to the interpreter.
    Stray(Type),
}

/// What [`Lowering::rewind`] restores.
#[derive(Clone)]
struct Checkpoint {
    scopes: Vec<Scope>,
    variables: Vec<Variable>,
    loops: usize,
    inlined: Vec<Inlined>,
    speculations: usize,
    nesting: usize,
    control: usize,
    regions: usize,
    unusable: usize,
    returned: Option<Type>,
    constructed: usize,
    r_calls: usize,
    blocks: usize,
    self_inlines: usize,
    pending: Vec<SmallVec<[Local; 4]>>,
    param_reads: usize,
    reassigned: usize,
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
    /// The `if` and `match` calls whose value is the function's result.
    tails: SmallVec<[Sexp; 2]>,
    /// The function's body.
    body: Sexp,
    callees: &'c mut dyn Callees,
    /// The function's own names, then one scope per inlined function.
    scopes: Vec<Scope>,
    /// The id the next scope takes.
    scope_ids: u32,
    variables: Vec<Variable>,
    /// The `next` and `break` targets of the loops around the current point.
    loops: Vec<(BlockId, BlockId)>,
    inlined: Vec<Inlined>,
    /// The typed functions whose bodies are being lowered into this one, outermost first, so a
    /// function that calls itself through another is called, not inlined without end.
    inlining: SmallVec<[Sexp; 4]>,
    /// How many times the function has been lowered into itself.
    self_inlines: usize,
    /// Whether every copy of the function lowered into itself here stands for a call in tail
    /// position, so a self call in tail position of this copy is one the interpreter would run
    /// in the frame of the outermost call ([`Function::tail_calls`]).
    tail_chain: bool,
    /// The typed functions that are called, never inlined.
    called: &'c [Sexp],
    /// The innermost typed function whose body was refused where it inlined.
    refused: &'c mut Option<Sexp>,
    /// Names whose variables, and step functions whose walk states, hold at least these tags
    /// ([`lower`]).
    widened: &'c [(Sexp, u32)],
    /// The name or step function and the union of tags the lowering needs a variable or a walk's
    /// state to hold, when it refused for it.
    widen: &'c mut Option<(Sexp, u32)>,
    speculations: Vec<Speculation>,
    nesting: usize,
    /// How many branches and loop bodies enclose the current point in the current scope.
    control: usize,
    /// The names of the current scope where each enclosing branch or loop body starts,
    /// innermost last, which [`Lowering::leave_control`] restores.
    regions: Vec<FxHashMap<Sexp, Binding>>,
    /// The results of an `if` whose branches have different types, which nothing may read.
    unusable: SmallVec<[Local; 4]>,
    /// What the matches over any tag relied on.
    any_tag: Option<AnyTag>,
    /// The type an early `return` of the function itself gives, the union of the tags when it
    /// returns tags.
    returned: Option<Type>,
    /// The type of the first branch of a result `if` that lowered, a guess for a recursive
    /// call's result when lowering is refused.
    tail_branch: Option<Type>,
    /// The tag shapes the code constructs, which a `match` over any tag also tests for.
    constructed: SmallVec<[u32; 4]>,
    r_calls: Vec<RCall>,
    /// How the value of the call lowered next is used, when that call may be an R call: as a
    /// statement whose value is unused, or through a conversion. Every call takes it.
    r_use: Option<RUse>,
    /// The destination of the last converted R call, which tells the conversion that its
    /// argument was one.
    converted: Option<Local>,
    /// The item type the checker gave the `List.from_r` or sibling lowered next
    /// ([`Conversion::checked`]).
    checked_items: Option<Item>,
    /// Whether the mapping call lowered next sits in a `.ry_items`, whose run-time function
    /// gives an empty result the checker's storage in the interpreter too, so the native code
    /// builds the empty list instead of stopping. The call takes it before lowering anything.
    settled_empty: bool,
    /// Each empty list literal of records or tags whose item type the code has not shown yet,
    /// with the locals that hold it. The preflight names only the storage of such a list, so it
    /// starts as a list of any tag and takes the item type of the first item appended to it or
    /// list assigned over it ([`Lowering::settle`]).
    pending: Vec<SmallVec<[Local; 4]>>,
    /// The name the last read refused for because the function sets it only later, with the
    /// number of scopes at the read, for the loop around the read to declare
    /// ([`Lowering::looped`]).
    forward: Option<(Sexp, usize)>,
    /// The names each loop at the function's own level declares before it, from the first time
    /// it lowered outside a trial, so lowering an enclosing loop again repeats no trials.
    declared_loops: Vec<(Sexp, SmallVec<[Declared; 2]>)>,
    /// How many loop trials enclose the current point: a trial lowers a loop only to learn the
    /// types of the names it sets, and leaves out each statement that reads one too early.
    probing: usize,
    /// The names each captured function sees ([`Callable::Captured`]).
    captures: Vec<FxHashMap<Sexp, Binding>>,
    /// The variable each parameter is bound to, by position.
    param_variables: SmallVec<[Local; 4]>,
    /// The formals of inlined local functions whose arguments read parameters, with those
    /// parameters: R forces an argument where the body first reads the formal, so each read of
    /// the formal counts as a read of them ([`Lowering::attribute_reads`]).
    param_aliases: Vec<(Local, SmallVec<[u32; 2]>)>,
    /// The parameters the arguments of the local function about to inline read, by formal,
    /// which [`Lowering::inline`] gives the formals' variables as aliases.
    formal_reads: SmallVec<[SmallVec<[u32; 2]>; 4]>,
    /// The variables of parameters and aliases assigned outside any branch or loop, whose later
    /// reads read the assigned value and force no argument.
    reassigned: Vec<Local>,
    /// Where the body reads a parameter by name ([`reads_before`]).
    param_reads: Vec<ParamRead>,
}

/// A name a loop declares before it ([`Lowering::looped`]): its symbol, its type, and whether
/// it is a pending empty list.
type Declared = (Sexp, Type, bool);

/// Where typed code may call an R function: where the value is unused, where a conversion
/// gives it a ry type, as `stop()`, which never returns, or anywhere when the function is one of
/// the table API's that gives a table value.
#[derive(Clone, Copy)]
enum RUse {
    Unused,
    /// Through a conversion, with the local holding a `List.from_r_or`'s default.
    Converted(Conversion, Option<Local>),
    Diverges,
    Table,
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

    /// A scope whose code starts here.
    fn new_scope(&mut self) -> Scope {
        let id = self.scope_ids;
        self.scope_ids += 1;
        let current = self.builder.current;
        Scope {
            names: FxHashMap::default(),
            id,
            entry: (
                current,
                self.builder.function.blocks[current.0 as usize].insts.len(),
            ),
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
                "a value whose type depends on the branch an `if` or `match` takes",
            ));
        }
        Ok(())
    }

    /// Enters a branch or a loop body, which may not run, giving the region to hand back to
    /// [`Lowering::leave_control`].
    fn enter_control(&mut self) -> usize {
        self.control += 1;
        let names = self
            .scopes
            .last()
            .map(|scope| scope.names.clone())
            .unwrap_or_default();
        self.regions.push(names);
        self.regions.len() - 1
    }

    /// Leaves a branch or loop body: the names it created go out of scope, and come back as a
    /// list for the caller to bind as an `if` or a loop leaves them ([`Lowering::join_names`],
    /// [`Lowering::leave_loop`]).
    fn leave_control(&mut self, region: usize) -> Created {
        self.control -= 1;
        debug_assert!(
            region + 1 == self.regions.len(),
            "regions leave in nesting order"
        );
        let saved = self
            .regions
            .get_mut(region)
            .map(std::mem::take)
            .unwrap_or_default();
        self.regions.truncate(region);
        let Some(scope) = self.scopes.last_mut() else {
            return SmallVec::new();
        };
        let mut created: Created = scope
            .names
            .iter()
            .filter(|(symbol, _)| !saved.contains_key(symbol))
            .filter_map(|(&symbol, &binding)| match binding {
                Binding::Value(local) => Some((symbol, local, false)),
                Binding::Maybe(local) => Some((symbol, local, true)),
                Binding::Function(..) | Binding::Hidden => None,
            })
            .collect();
        // A join emits copies in this order, which must not depend on hashing.
        created.sort_by_key(|&(_, local, _)| local);
        let hidden: SmallVec<[Sexp; 2]> = scope
            .names
            .iter()
            .filter(|&(symbol, binding)| {
                !saved.contains_key(symbol)
                    && matches!(binding, Binding::Function(..) | Binding::Hidden)
            })
            .map(|(&symbol, _)| symbol)
            .collect();
        scope.names = saved;
        for symbol in hidden {
            scope.names.insert(symbol, Binding::Hidden);
        }
        created
    }

    /// Binds the names that the completing ends of an `if` or `match` created, each an end block
    /// with its list. A name every end sets in one variable keeps it, and so does a name only some
    /// ends set, as a [`Binding::Maybe`]. A name the ends set in variables of different types that
    /// join gets a variable of the joined type, which each end copies into; any other name is out
    /// of scope after the join.
    fn join_names(&mut self, ends: &[(BlockId, Created)]) {
        for symbol in created_symbols(ends) {
            let found = created_in(ends, symbol);
            match self.name_join(&found) {
                None => {}
                Some(NameJoin::Shared(binding)) => self.bind(symbol, binding),
                Some(NameJoin::Copied(ty)) => {
                    let variable = self.variable(symbol, ty);
                    for (&(end, _), &local) in ends.iter().zip(&found) {
                        let Some((local, _)) = local else {
                            continue;
                        };
                        self.builder.switch_to(end);
                        self.builder.push(Some(variable), Op::Copy(local));
                    }
                    self.bind(symbol, Binding::Value(variable));
                }
            }
        }
    }

    /// Whether joining `ends`, the arms of the `match` call `call`, leaves a name some end
    /// created unbound, because its ends give it types that do not join, where code outside the
    /// call reads it. Inside an inlined function every such name counts.
    unsafe fn drops_a_name(&self, ends: &[(BlockId, Created)], call: Sexp) -> bool {
        created_symbols(ends).into_iter().any(|symbol| {
            self.name_join(&created_in(ends, symbol)).is_none()
                && (!self.inlined.is_empty() || mentions_outside(self.body, call, symbol))
        })
    }

    /// How a name joins after branches, given the variable each end left it in and whether it
    /// may be unset there: `None` when it cannot be read after them.
    fn name_join(&self, found: &[Option<(Local, bool)>]) -> Option<NameJoin> {
        let &Some((first, _)) = found.iter().find(|end| end.is_some())? else {
            return None;
        };
        if found.iter().flatten().all(|&(local, _)| local == first) {
            let everywhere = found.iter().all(|end| matches!(end, Some((_, false))));
            return Some(NameJoin::Shared(if everywhere {
                Binding::Value(first)
            } else {
                Binding::Maybe(first)
            }));
        }
        // Variables of different types: the join copies them into one of the joined type,
        // which needs the name set, as a value, on every end.
        let mut ty: Option<Type> = None;
        for end in found {
            let Some((local, false)) = *end else {
                return None;
            };
            let found = self.builder.ty(local);
            ty = Some(match ty {
                None => found,
                Some(held) => joined(held, found)?,
            });
        }
        ty.map(NameJoin::Copied)
    }

    /// Binds the names a loop body created: after the loop each is set only if the body ran far
    /// enough to set it.
    fn leave_loop(&mut self, created: Created) {
        for (symbol, local, _) in created {
            self.bind(symbol, Binding::Maybe(local));
        }
    }

    /// `ty` holding the tags an earlier lowering found `key` needs ([`lower`]).
    fn widened_type(&self, key: Sexp, ty: Type) -> Type {
        match self.widened.iter().find(|&&(held, _)| held == key) {
            Some(&(_, wider)) => with_tags(ty, wider),
            None => ty,
        }
    }

    /// Whether a value of `found` may be stored where `held` is, which holds for the same type
    /// and for fewer tags. A value with more tags asks [`lower`] to widen `key`.
    fn fits_widening(&mut self, key: Sexp, held: Type, found: Type) -> bool {
        match (held_tags(held), held_tags(found)) {
            (Some(held_union), Some(found_union))
                if with_tags(held, found_union) == with_tags(found, held_union) =>
            {
                let fits = shapes::within(found_union, held_union);
                if !fits {
                    *self.widen = Some((key, shapes::join(held_union, found_union)));
                }
                fits
            }
            _ => held == found,
        }
    }

    /// The variable of `symbol` in the current scope that holds values of `ty`, made when the name
    /// has none yet. A pending empty list never shares one, since its type is a placeholder.
    fn variable(&mut self, symbol: Sexp, ty: Type) -> Local {
        let ty = self.widened_type(symbol, ty);
        let (scope, entry) = self
            .scopes
            .last()
            .map_or((0, (BlockId(0), 0)), |scope| (scope.id, scope.entry));
        let existing = self.variables.iter().rev().find(|variable| {
            variable.scope == scope
                && variable.symbol == symbol
                && self.builder.ty(variable.local) == ty
                && self.pending_family(variable.local).is_none()
        });
        if let Some(existing) = existing {
            return existing.local;
        }
        let local = self.builder.local(ty);
        self.variables.push(Variable {
            scope,
            entry,
            symbol,
            local,
            flag: None,
        });
        local
    }

    /// The flag of the variable `local`, made when it has none yet.
    fn flag(&mut self, local: Local) -> Result<Local, Refusal> {
        let index = self
            .variables
            .iter()
            .rposition(|variable| variable.local == local)
            .ok_or(Refusal::new(
                "a name set on some paths only, outside its variable",
            ))?;
        if let Some(flag) = self.variables[index].flag {
            return Ok(flag);
        }
        let flag = self.builder.local(Type::Bool);
        self.variables[index].flag = Some(flag);
        Ok(flag)
    }

    /// Gives every flag its value: false where its variable's scope starts, and after each write
    /// of one of the name's variables, true for that variable and false for the others.
    fn maintain_flags(&mut self) {
        let variables = &self.variables;
        let entries: SmallVec<[((BlockId, usize), Local); 4]> = variables
            .iter()
            .filter_map(|variable| Some((variable.entry, variable.flag?)))
            .collect();
        if entries.is_empty() {
            return;
        }
        let same_name = |left: &Variable, right: &Variable| {
            left.scope == right.scope && left.symbol == right.symbol
        };
        // The variable each local is, for the names one of whose variables has a flag.
        let mut written: Vec<Option<u32>> = vec![None; self.builder.function.locals.len()];
        for (index, variable) in variables.iter().enumerate() {
            if variables
                .iter()
                .any(|other| other.flag.is_some() && same_name(other, variable))
            {
                written[variable.local.0 as usize] = Some(index as u32);
            }
        }
        for (number, block) in self.builder.function.blocks.iter_mut().enumerate() {
            let old = std::mem::take(&mut block.insts);
            let mut insts = Vec::with_capacity(old.len());
            let count = old.len();
            let mut old = old.into_iter();
            for position in 0..=count {
                for &(_, flag) in entries
                    .iter()
                    .filter(|&&(entry, _)| entry == (BlockId(number as u32), position))
                {
                    insts.push(Inst::new(Some(flag), Op::Const(Constant::Lgl(0))));
                }
                let Some(inst) = old.next() else {
                    break;
                };
                let writes: SmallVec<[u32; 2]> = inst
                    .assigned()
                    .iter()
                    .filter_map(|local| written.get(local.0 as usize).copied().flatten())
                    .collect();
                insts.push(inst);
                for index in writes {
                    let variable = &variables[index as usize];
                    for other in variables.iter().filter(|other| same_name(other, variable)) {
                        if let Some(flag) = other.flag {
                            let set = i32::from(other.local == variable.local);
                            insts.push(Inst::new(Some(flag), Op::Const(Constant::Lgl(set))));
                        }
                    }
                }
            }
            block.insts = insts;
        }
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

    /// The parameters a read of `local` forces: its own if it is a parameter's variable, or those
    /// its argument read if it is the formal of an inlined local function.
    fn forced_by(&self, local: Local) -> SmallVec<[u32; 2]> {
        if self.reassigned.contains(&local) {
            return SmallVec::new();
        }
        match self
            .param_variables
            .iter()
            .position(|&variable| variable == local)
        {
            Some(param) => SmallVec::from_slice(&[param as u32]),
            None => self
                .param_aliases
                .iter()
                .find(|(alias, _)| *alias == local)
                .map(|(_, params)| params.clone())
                .unwrap_or_default(),
        }
    }

    /// Takes the parameter reads made computing the arguments `values` of a local function out
    /// of [`Lowering::param_reads`], into [`Lowering::formal_reads`] by formal, where `spans`
    /// gives the reads of each argument. R forces an argument where the function first reads its
    /// formal, so they count there. A `strict` call, of an annotated function from annotated
    /// code, forces its arguments at the call as the interpreter does, except a lazy one
    /// ([`ArgumentReads::lazy`]). A read passed into an R call stays, as native code makes that
    /// call at the call of the function (earlier than R), and so do the reads of a passed
    /// function. Moving reads made on some paths only would count them on every path, so an
    /// argument with such reads declines.
    fn attribute_reads(
        &mut self,
        values: &[Argument],
        spans: &[ArgumentReads],
        strict: bool,
    ) -> Result<(), Refusal> {
        let mut moved: SmallVec<[usize; 8]> = SmallVec::new();
        let mut formal_reads = SmallVec::new();
        for (value, span) in values.iter().zip(spans) {
            let mut params: SmallVec<[u32; 2]> = SmallVec::new();
            if let Argument::Value(_) = value
                && (span.lazy || !strict)
            {
                if self.argument_paths(span, &[]).escapes {
                    return Err(Refusal::new(
                        "an argument of a local function leaves through `return`, `break` or \
                         `next`, which R runs where the function reads the argument: compute the \
                         argument into a variable first",
                    ));
                }
                let reads = self.param_reads.get(span.start..span.end).unwrap_or(&[]);
                let mut everywhere: SmallVec<[u32; 2]> = SmallVec::new();
                let mut checked: SmallVec<[u32; 2]> = SmallVec::new();
                for read in reads {
                    if read.into.is_some() || checked.contains(&read.param) {
                        continue;
                    }
                    checked.push(read.param);
                    let blocks: SmallVec<[BlockId; 4]> = reads
                        .iter()
                        .filter(|other| other.into.is_none() && other.param == read.param)
                        .map(|other| other.block)
                        .collect();
                    if blocks.contains(&span.entry)
                        || blocks.contains(&span.exit)
                        || !self.argument_paths(span, &blocks).reaches_exit
                    {
                        everywhere.push(read.param);
                    }
                }
                let everywhere = |param: u32| everywhere.contains(&param);
                for index in span.start..span.end {
                    if let Some(read) = self.param_reads.get(index)
                        && read.into.is_none()
                    {
                        if !everywhere(read.param) {
                            return Err(Refusal::new(
                                "it reads a parameter on some paths of an argument of a local \
                                 function, which R computes where the function reads it: compute \
                                 the argument into a variable first",
                            ));
                        }
                        if !params.contains(&read.param) {
                            params.push(read.param);
                        }
                        moved.push(index);
                    }
                }
            }
            formal_reads.push(params);
        }
        self.formal_reads = formal_reads;
        moved.sort_unstable();
        moved.dedup();
        let mut index = 0;
        self.param_reads.retain(|_| {
            index += 1;
            moved.binary_search(&(index - 1)).is_err()
        });
        Ok(())
    }

    /// Where the paths of the argument `span` that run none of `avoid` go from its entry.
    fn argument_paths(&self, span: &ArgumentReads, avoid: &[BlockId]) -> ArgumentPaths {
        let blocks = &self.builder.function.blocks;
        let mut paths = ArgumentPaths {
            reaches_exit: false,
            escapes: false,
        };
        let mut seen = vec![0u64; blocks.len().div_ceil(64)];
        let mut stack: SmallVec<[BlockId; 8]> = SmallVec::from_slice(&[span.entry]);
        while let Some(id) = stack.pop() {
            let index = id.0 as usize;
            if id == span.exit {
                paths.reaches_exit = true;
                continue;
            }
            if id != span.entry && id.0 < span.first_block {
                paths.escapes = true;
                continue;
            }
            if avoid.contains(&id) || seen[index / 64] & (1 << (index % 64)) != 0 {
                continue;
            }
            seen[index / 64] |= 1 << (index % 64);
            if let Some(block) = blocks.get(index) {
                if let Terminator::Return(_) = block.terminator {
                    paths.escapes = true;
                }
                stack.extend(block.terminator.successors());
            }
        }
        paths
    }

    /// Where lowering stands before an argument of a local function, which
    /// [`Lowering::argument_reads`] completes.
    fn argument_start(&self) -> ArgumentReads {
        ArgumentReads {
            start: self.param_reads.len(),
            end: self.param_reads.len(),
            entry: self.builder.current,
            exit: self.builder.current,
            first_block: self.builder.function.blocks.len() as u32,
            lazy: false,
            first_r_call: self.r_calls.len(),
            end_r_call: self.r_calls.len(),
        }
    }

    /// The reads of the argument lowered since `start`.
    fn argument_reads(&self, start: ArgumentReads, lazy: bool) -> ArgumentReads {
        ArgumentReads {
            end: self.param_reads.len(),
            exit: self.builder.current,
            lazy,
            end_r_call: self.r_calls.len(),
            ..start
        }
    }

    unsafe fn read(&mut self, symbol: Sexp) -> Result<Local, Refusal> {
        match self.lookup(symbol) {
            Some(Binding::Value(local)) => {
                let block = self.builder.current;
                let position = self
                    .builder
                    .function
                    .blocks
                    .get(block.0 as usize)
                    .map_or(0, |block| block.insts.len());
                for param in self.forced_by(local) {
                    self.param_reads.push(ParamRead {
                        block,
                        position: position as u32,
                        param,
                        into: None,
                    });
                }
                Ok(local)
            }
            Some(Binding::Maybe(local)) => {
                // Where the name is unset, R reads it from the enclosing environment or fails,
                // which the interpreter does when the call gives up.
                let flag = self.flag(local)?;
                let (set, unset) = (self.builder.block(), self.builder.block());
                self.builder.terminate(Terminator::Branch {
                    condition: flag,
                    then: set,
                    otherwise: unset,
                });
                self.builder.switch_to(set);
                if let Some(scope) = self.scopes.last_mut()
                    && let Some(binding) = scope.names.get_mut(&symbol)
                {
                    *binding = Binding::Value(local);
                }
                Ok(local)
            }
            Some(Binding::Function(..)) => Err(Refusal::new("a local function used as a value")),
            Some(Binding::Hidden) => Err(hidden_refusal()),
            None => self.library_constant(symbol),
        }
    }

    /// A read of `symbol` from outside the function: a constant of the standard library, such
    /// as `Num.pi`, read as its value for as long as the lookup finds that binding, or else a
    /// global that holds a ry value.
    unsafe fn library_constant(&mut self, symbol: Sexp) -> Result<Local, Refusal> {
        self.read_before_assignment(symbol)?;
        // `package:std` binds the very object the builtins environment was created with, and
        // that object is shared, so nothing modifies it in place.
        let original = interp::standard_library::builtins_env()
            .and_then(|env| interp::heap().frame_cell(env, symbol))
            .map(|cell| cell.cdr());
        match resolve_value(symbol, self.env) {
            Some(speculation) if original == Some(speculation.function) => {
                let local = self.constant(speculation.function)?;
                self.speculations.push(speculation);
                Ok(local)
            }
            _ => self.global(symbol),
        }
    }

    /// Refuses a read of `symbol` from outside the function where the function assigns it.
    /// Locals are bound as lowering reaches their assignment, so a read that a loop runs again
    /// after a later assignment of the same name would keep reading the outside one; the
    /// lowering then runs again with the name declared before the loop.
    unsafe fn read_before_assignment(&mut self, symbol: Sexp) -> Result<(), Refusal> {
        if assigns(interp::bytecode::closure_body(self.closure), symbol)
            || self
                .inlining
                .iter()
                .any(|&function| assigns(interp::bytecode::closure_body(function), symbol))
        {
            self.forward = Some((symbol, self.scopes.len()));
            return Err(Refusal(Cow::Owned(format!(
                "it reads `{}` from outside the function before it assigns it: give the local \
                 another name",
                rvalues::symbol_name(symbol)
            ))));
        }
        Ok(())
    }

    /// A read of the global `symbol`, which holds a ry value. The code reads the binding each
    /// time it runs, so it sees a rebinding, and the value must keep the type it holds here.
    unsafe fn global(&mut self, symbol: Sexp) -> Result<Local, Refusal> {
        let name = rvalues::symbol_name(symbol);
        let value = match global_binding(symbol, self.env) {
            Global::Value(value) => value,
            Global::Unbound => {
                AWAITED.set(Some(Awaited::Binding(symbol, self.env)));
                return Err(Refusal(Cow::Owned(format!(
                    "{AWAITING}{name} <- ...` once, which binds the global `{name}` it reads"
                ))));
            }
            Global::Enclosing => {
                return Err(Refusal(Cow::Owned(format!(
                    "it reads `{name}` from an enclosing function: pass the value in as a \
                     parameter"
                ))));
            }
            Global::Promise(promise) => {
                AWAITED.set(Some(Awaited::Promise(promise)));
                return Err(Refusal(Cow::Owned(format!(
                    "{AWAITING}{name}` once, which loads the value of the global"
                ))));
            }
            Global::Active => {
                return Err(Refusal(Cow::Owned(format!(
                    "it reads `{name}`, an active binding, which runs R code when it is read: \
                     pass the value in as a parameter"
                ))));
            }
        };
        let ty = crate::type_of(value).filter(|&ty| reads_globally(ty)).ok_or_else(|| {
            Refusal(Cow::Owned(format!(
                "it reads the global `{name}`, which holds a value native code does not read from \
                 a global: pass the value in as a parameter"
            )))
        })?;
        Ok(self.r_site(
            symbol,
            rvalues::nil(),
            (RUse::Unused, RResult::Global(ty)),
            true,
            SmallVec::new(),
            SmallVec::new(),
        ))
    }

    unsafe fn assign(&mut self, symbol: Sexp, value: Local) -> Result<Local, Refusal> {
        let ty = self.builder.ty(value);
        let assignable = match self
            .scopes
            .last()
            .and_then(|scope| scope.names.get(&symbol))
        {
            // A name that may be unset may also take a value of another type, in another
            // variable, whose write clears this one's flag. A pending empty list takes the
            // variable's item type, or gives it its own.
            Some(&Binding::Maybe(local)) => {
                let pending =
                    self.pending_family(local).is_some() || self.pending_family(value).is_some();
                let lists = matches!(
                    (self.builder.ty(local), ty),
                    (
                        Type::Vector { array: true, .. },
                        Type::Vector { array: true, .. }
                    )
                );
                (self.builder.ty(local) == ty || (pending && lists)).then_some(local)
            }
            _ => self.lookup_assignable(symbol),
        };
        if let Some(variable) = assignable {
            self.unify_pending(variable, value)?;
            let held = self.builder.ty(variable);
            let ty = self.builder.ty(value);
            if self.fits_widening(symbol, held, ty) {
                if variable != value {
                    self.builder.push(Some(variable), Op::Copy(value));
                }
                if self.control == 0 && !self.forced_by(variable).is_empty() {
                    self.reassigned.push(variable);
                }
                self.bind(symbol, Binding::Value(variable));
                return Ok(variable);
            }
            // A value of another type goes into a variable of its own type, which is sound
            // wherever no code after it can read the name's old variable: outside any branch,
            // loop or `while` condition, or for a name the innermost one bound, which goes out
            // of scope as it ends. A tag union that needs more tags restarts wider instead.
            let widening = matches!(
                (held_tags(held), held_tags(ty)),
                (Some(held_union), Some(found_union))
                    if with_tags(held, found_union) == with_tags(ty, held_union)
            );
            let entered = self.control > 0
                && self
                    .regions
                    .last()
                    .is_some_and(|names| names.contains_key(&symbol));
            if widening || entered {
                return Err(Refusal::new("a variable that changes type"));
            }
        }
        let ty = self.builder.ty(value);
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
        let family = self.pending_family(value);
        // A pending empty list gets a variable of its own, never shared, since its type is a
        // placeholder; it still counts as one of the name's variables for the set flags.
        let variable = match family {
            Some(_) => {
                let local = self.builder.local(ty);
                let (scope, entry) = self
                    .scopes
                    .last()
                    .map_or((0, (BlockId(0), 0)), |scope| (scope.id, scope.entry));
                self.variables.push(Variable {
                    scope,
                    entry,
                    symbol,
                    local,
                    flag: None,
                });
                local
            }
            None => self.variable(symbol, ty),
        };
        self.builder.push(Some(variable), Op::Copy(value));
        if let Some(family) = family {
            self.pending[family].push(variable);
        }
        self.bind(symbol, Binding::Value(variable));
        Ok(variable)
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
        let read = function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|inst| {
                inst.op
                    .operands()
                    .iter()
                    .any(|operand| members.contains(operand))
                    && !matches!(inst.op, Op::Copy(_) | Op::Length(_) | Op::Relabel(..))
            });
        if read {
            return Err(Refusal::new(
                "an empty list read before anything shows its item type",
            ));
        }
        retype_family(function, &members, item);
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
        self.append_value(list, value)
    }

    /// `list` with `value` appended, in place when the list is uniquely held.
    unsafe fn append_value(&mut self, list: Local, value: Local) -> Result<Local, Refusal> {
        self.usable(list)?;
        self.usable(value)?;
        let item = self
            .builder
            .ty(value)
            .held_item()
            .ok_or(Refusal::new("an append of something a list cannot hold"))?;
        self.settle(list, item)?;
        let ty = self.builder.ty(list);
        // A tag of another shape widens the list's item union: its items are boxes either way.
        let ty = match (ty, item) {
            (
                Type::Vector {
                    item: Item::Tag(held),
                    array: true,
                },
                Item::Tag(found),
            ) => with_tags(ty, shapes::join(held, found)),
            (
                Type::Vector {
                    item: held,
                    array: true,
                },
                found,
            ) if held == found => ty,
            (Type::Vector { array: true, .. }, _) => {
                return Err(Refusal::new("an append of an item of another type"));
            }
            _ => return Err(Refusal::new("an append to something other than a ry list")),
        };
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
    /// `List.concat`, or `List.concat_utf8` as the concatenation with the string's bytes.
    unsafe fn concat(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let [first, second] = arguments.exactly(2)?[..] else {
            return Err(Refusal::new("a malformed `List.concat`"));
        };
        let first = self.value(first)?;
        let mut second = self.value(second)?;
        if name == "List.concat_utf8" {
            if self.builder.ty(second) != Type::Str {
                return Err(Refusal::new(
                    "a `List.concat_utf8` of something other than a string",
                ));
            }
            second = self.encode(Encoding::Utf8, second);
        }
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
        let dropped = matches!(r_use, Some(RUse::Unused));
        // The language's own forms, unless the program rebinds them.
        match name.as_str() {
            "{" => return self.sequence(call, dropped),
            "(" => {
                let [inner] = self.arguments(call, 1)?[..] else {
                    return Err(Refusal::new("a malformed parenthesis"));
                };
                return self.part(inner, dropped);
            }
            "<-" | "=" => return self.assignment(call).map(Some),
            "if" => return self.conditional(call, dropped),
            "for" => return self.looped(call, Self::for_loop),
            "while" => return self.looped(call, Self::while_loop),
            "repeat" => return self.looped(call, Self::repeat_loop),
            ".ry_try" => return self.try_result(call),
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
                let raised = match find_function(head, self.env) {
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
        if let Some(Binding::Function(callable)) = self.lookup(head) {
            let arguments = Arguments::of(call);
            let mut spans = SmallVec::new();
            let values = match callable {
                Callable::Inline(formals, body, scope) => {
                    let frame = Frame::Local(scope + 1, body);
                    self.matched_arguments(formals, &arguments, frame, &mut spans)?
                }
                Callable::Typed(_, function) => self.matched_arguments(
                    function.car(),
                    &arguments,
                    Frame::Typed(function),
                    &mut spans,
                )?,
                Callable::Captured(..) | Callable::Library(_)
                    if arguments.items.iter().any(|(tag, _)| tag.is_some()) =>
                {
                    return Err(Refusal::new(
                        "a function passed to a parameter called with named arguments",
                    ));
                }
                Callable::Captured(..) | Callable::Library(_) => {
                    self.passed_arguments(&arguments, &mut spans)?
                }
            };
            if let Callable::Inline(formals, body, _) | Callable::Captured(formals, body, _) =
                callable
            {
                let strict = self.inlined.last().is_none_or(|inlined| inlined.typed)
                    && typed_definition(formals, body);
                self.attribute_reads(&values, &spans, strict)?;
                lazy_r_calls(formals, body, &values, &spans, strict)?;
            }
            return self.apply_arguments(callable, &values);
        }
        match self.lookup(head) {
            Some(Binding::Hidden) => return Err(hidden_refusal()),
            Some(_) => return Err(Refusal::new("a call of a local variable")),
            None => {}
        }
        let Some(speculation) = find_function(head, self.env) else {
            return Err(Refusal(Cow::Owned(format!(
                "it calls `{name}`, which no binding defines as a function"
            ))));
        };
        let function = speculation.function;
        if is_base_function(&speculation) {
            self.speculations.push(speculation);
            if name == ".ry_match" {
                return self.match_expression(call, dropped);
            }
            self.r_use = r_use;
            return self.builtin(&name, call, function).map(Some);
        }
        if is_table_function(&speculation) {
            self.speculations.push(speculation);
            return self.r_call(call, function, RUse::Table).map(Some);
        }
        if function.kind() == SexpType::Closure && interp::annotations::is_typed(function) {
            self.speculations.push(speculation);
            let arguments = Arguments::of(call);
            let values = self.matched_arguments(
                function.car(),
                &arguments,
                Frame::Typed(function),
                &mut SmallVec::new(),
            )?;
            // The interpreter runs a self call in tail position without a new frame, so native
            // code may loop there and nowhere else.
            let tail = function == self.closure
                && self.tail_chain
                && in_tail_position(
                    Gc::adopt(interp::bytecode::closure_body(self.closure)),
                    Gc::adopt(call),
                );
            return self.typed_function(head, function, &values, tail);
        }
        if let Some(r_use) = r_use {
            self.speculations.push(speculation);
            return self.r_call(call, function, r_use).map(Some);
        }
        Err(r_value_refusal(head))
    }

    /// A call of the R function `function`, whose value is used as `r_use` says.
    unsafe fn r_call(&mut self, call: Sexp, function: Sexp, r_use: RUse) -> Result<Local, Refusal> {
        if let Some(symbol) = called_symbol(call.car())
            && uses_its_frame(&rvalues::symbol_name(symbol))
            && is_base_value(symbol, function)
        {
            return Err(Refusal(Cow::Owned(format!(
                "a call of R's `{}`, which looks at the frame it is called from",
                rvalues::symbol_name(symbol)
            ))));
        }
        let name = || interp::deparse::deparse_first_line(call.car());
        let result = r_result(r_use)?;
        let table = matches!(r_use, RUse::Table);
        let calls = self.r_calls.len();
        let reads = self.param_reads.len();
        let mut arguments: SmallVec<[RArgument; 4]> = SmallVec::new();
        let mut values: SmallVec<[Local; 4]> = SmallVec::new();
        for (tag, code) in rvalues::pairlist_items(call.cdr()) {
            if code == rvalues::symbols().dots || code == rvalues::missing_arg() {
                return Err(Refusal(Cow::Owned(format!(
                    "it calls R's `{}` with `...` or an empty argument",
                    name()
                ))));
            }
            if self.passes_code(code) {
                arguments.push(RArgument {
                    tag,
                    code,
                    passed: Passed::Code,
                });
                continue;
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
                passed: Passed::Value(ty, conversion),
            });
            values.push(value);
        }
        // R evaluates arguments lazily, in the order the function asks for them, and compiled
        // code computes them first, which only agrees while computing them calls no R function
        // that could tell.
        if self.r_calls[calls..].iter().any(|site| !site.effect_free) {
            return Err(Refusal(Cow::Owned(format!(
                "it calls R inside the arguments of R's `{}`, whose order R's lazy arguments \
                 decide: compute the inner call into a variable first",
                name()
            ))));
        }
        // A base builtin, or a base function that only formats or reports what it is passed,
        // forces its arguments before it runs any R code of the caller's, which only a variable
        // or a call passed as code could bring, so its arguments count as read at the call.
        let forced_at_call = called_symbol(call.car()).is_some_and(|symbol| {
            is_base_value(symbol, function)
                && (function.kind() == SexpType::Builtin
                    || FORCING_BASE_FUNCTIONS.contains(&&*rvalues::symbol_name(symbol)))
        }) && arguments.iter().all(|argument| {
            !matches!(argument.passed, Passed::Code)
                || !matches!(
                    argument.code.kind(),
                    SexpType::Language | SexpType::Symbol | SexpType::Promise
                )
        });
        let local = self.r_site(call, function, (r_use, result), table, arguments, values);
        if !forced_at_call {
            let site = self.r_calls.len() - 1;
            for read in self.param_reads.iter_mut().skip(reads) {
                read.into = Some(site);
            }
        }
        Ok(local)
    }

    /// Whether the argument `code` of an R call passes as code for R to evaluate: a literal, or
    /// a variable that is not one of the function's own. R evaluates either as it would in the
    /// interpreter, so neither needs a ry type. A variable the function assigns somewhere is
    /// left to the lookup, which finds the local once its assignment is lowered, because a read
    /// that a loop runs again after that assignment reads the local.
    unsafe fn passes_code(&self, code: Sexp) -> bool {
        match code.kind() {
            SexpType::Integer
            | SexpType::Real
            | SexpType::Logical
            | SexpType::String
            | SexpType::Complex
            | SexpType::Nil => true,
            SexpType::Symbol => {
                self.lookup(code).is_none()
                    && !assigns(interp::bytecode::closure_body(self.closure), code)
                    && !self
                        .inlining
                        .iter()
                        .any(|&function| assigns(interp::bytecode::closure_body(function), code))
            }
            // A call of a base R function on such code, which R evaluates lazily, as the
            // interpreter would. The checker accepts the same calls (`is_r_code` in
            // `semantics::native`).
            SexpType::Language => {
                let head = code.car();
                let callable = if head.kind() == SexpType::Symbol {
                    let name = rvalues::symbol_name(head);
                    (name == "("
                        || (takes_r_arguments(&name)
                            && !is_language_keyword(&name)
                            && !reads_its_frame(&name)))
                        && self.passes_code(head)
                        && self.binds_in_base(head)
                } else {
                    // `base::f`, which R resolves when the promise runs.
                    head.kind() == SexpType::Language
                        && head.car().kind() == SexpType::Symbol
                        && matches!(&*rvalues::symbol_name(head.car()), "::" | ":::")
                        && match rvalues::pairlist_items(head.cdr())[..] {
                            [(_, package), (_, function)] => {
                                package.kind() == SexpType::Symbol
                                    && &*rvalues::symbol_name(package) == "base"
                                    && function.kind() == SexpType::Symbol
                                    && !reads_its_frame(&rvalues::symbol_name(function))
                            }
                            _ => false,
                        }
                };
                callable
                    && rvalues::pairlist_items(code.cdr())
                        .into_iter()
                        .all(|(_, argument)| self.passes_code(argument))
            }
            _ => false,
        }
    }

    /// Whether R finds `symbol` from the function's environment in base R, whose functions do not
    /// look at the frame they are called from unless `reads_its_frame` names them. A function of
    /// another package may, as `glue::glue` does, and from a promise it would see the function's
    /// enclosing environment instead of its frame.
    unsafe fn binds_in_base(&self, symbol: Sexp) -> bool {
        let heap = interp::heap();
        let known = heap.known();
        let mut env = self.env;
        while env != known.empty_env {
            if heap.frame_cell(env, symbol).is_some() {
                return env == known.base_env || env == known.base_namespace;
            }
            env = env.enclosure();
        }
        false
    }

    /// Whether only R can evaluate the argument `code` of a base function: a literal with no ry
    /// type, such as `NA` or `2i`, a variable from outside the function, or a call of an R
    /// function on such code.
    unsafe fn only_r_evaluates(&self, code: Sexp) -> bool {
        match code.kind() {
            SexpType::Symbol | SexpType::Language => self.passes_code(code),
            SexpType::Complex => true,
            SexpType::Logical | SexpType::Integer => {
                rvalues::ints(code).first() == Some(&NA_INTEGER)
            }
            SexpType::Real => rvalues::reals(code)
                .first()
                .is_some_and(|&number| rvalues::is_na(number)),
            SexpType::String => rvalues::length(code) == 1 && rvalues::string_at(code, 0).is_none(),
            _ => false,
        }
    }

    /// Emits the call of the R function `function` with the arguments `arguments`, whose values
    /// are `values`.
    fn r_site(
        &mut self,
        call: Sexp,
        function: Sexp,
        (r_use, result): (RUse, RResult),
        effect_free: bool,
        arguments: SmallVec<[RArgument; 4]>,
        mut values: SmallVec<[Local; 4]>,
    ) -> Local {
        if let RUse::Converted(_, Some(default)) = r_use {
            values.push(default);
        }
        self.r_calls.push(RCall {
            function,
            call,
            env: self.env,
            arguments,
            result,
            diverges: matches!(r_use, RUse::Diverges),
            effect_free,
            read_before: 0,
        });
        let site = self.r_calls.len() - 1;
        match result {
            RResult::Unused => {
                self.builder.push(None, Op::CallR(site, values));
                self.builder.emit(Type::Null, Op::Const(Constant::Null))
            }
            RResult::Converted(_, ty) => {
                // A `List.from_r_missing` makes its results' shapes.
                if let Type::Vector {
                    item: Item::Tag(union),
                    ..
                } = ty
                {
                    for shape in shapes::members(union).unwrap_or_default() {
                        if !self.constructed.contains(&shape) {
                            self.constructed.push(shape);
                        }
                    }
                }
                let dest = self.builder.emit(ty, Op::CallR(site, values));
                self.converted = Some(dest);
                dest
            }
            RResult::Table => self.builder.emit(Type::Table, Op::CallR(site, values)),
            RResult::Global(ty) => self.builder.emit(ty, Op::CallR(site, values)),
        }
    }

    /// A read of the column `name` of the table `table`, as R's `[[` reads it. The column is an R
    /// vector, so the read stands where a call of R may.
    unsafe fn column(
        &mut self,
        call: Sexp,
        r_use: Option<RUse>,
        table: (Sexp, Local),
        name: (Sexp, Local),
    ) -> Result<Local, Refusal> {
        let Some(r_use) = r_use else {
            return Err(Refusal::new(
                "it uses a column of a table directly, and a column is an R vector: read it \
                 inside `List.from_r`, which gives it a ry type",
            ));
        };
        if self.builder.ty(name.1) != Type::Str {
            return Err(Refusal::new(
                "a column of a table read by something other than its name",
            ));
        }
        let read = rvalues::install("[[")
            .ok()
            .and_then(|symbol| find_function(symbol, self.env))
            .filter(|speculation| is_base_function(speculation))
            .ok_or(Refusal::new("`[[` is not R's own"))?;
        self.speculations.push(read);
        let result = r_result(r_use)?;
        let arguments = [table, name]
            .iter()
            .map(|&(code, value)| RArgument {
                tag: rvalues::nil(),
                code,
                passed: Passed::Value(self.builder.ty(value), None),
            })
            .collect();
        let values = SmallVec::from_slice(&[table.1, name.1]);
        Ok(self.r_site(
            call,
            read.function,
            (r_use, result),
            true,
            arguments,
            values,
        ))
    }

    /// Whether the code `code` gives a table value, as far as its form tells: a variable that
    /// holds one, a call of a table function, or an operator on one. Arithmetic lowers a whole
    /// tree of operators at once, so it has to know before it lowers an operand.
    unsafe fn is_table(&self, code: Sexp) -> bool {
        match code.kind() {
            SexpType::Symbol => matches!(
                self.lookup(code),
                Some(Binding::Value(local) | Binding::Maybe(local))
                    if self.builder.ty(local) == Type::Table
            ),
            SexpType::Language if code.car().kind() == SexpType::Symbol => {
                let head = code.car();
                let name = rvalues::symbol_name(head);
                if self.lookup(head).is_some() {
                    return false;
                }
                if name == "(" || is_operator(&name) {
                    return rvalues::pairlist_items(code.cdr())
                        .iter()
                        .any(|&(_, operand)| self.is_table(operand));
                }
                find_function(head, self.env).is_some_and(|speculation| {
                    is_table_function(&speculation) || returns_table(speculation.function)
                })
            }
            _ => false,
        }
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
        let speculation = find_function(code.car(), self.env)?;
        if !is_base_function(&speculation) {
            return None;
        }
        self.speculations.push(speculation);
        Some((
            inner,
            Conversion {
                call: code,
                function: speculation.function,
                checked: None,
                default: None,
            },
        ))
    }

    /// Calls the typed closure `function`, which `name` names, with `values`.
    ///
    /// A small function lowers into its caller, where its records need no allocation and its
    /// arguments no call. A function that calls itself from a place or two lowers into itself
    /// at those places, one level deep, so a recursion makes fewer calls, and into no other
    /// caller. The copies are counted as they are made, because a local function that holds
    /// the call is itself lowered at each of its calls.
    unsafe fn typed_function(
        &mut self,
        name: Sexp,
        function: Sexp,
        arguments: &[Argument],
        tail: bool,
    ) -> Result<Option<Local>, Refusal> {
        let body = interp::bytecode::closure_body(function);
        let Some(values) = plain_values(arguments) else {
            // Compiled code holds no function values, so a function passed to a parameter is
            // known where the call is lowered. The callee lowers into the caller with that
            // function in place of the parameter, once per call, unless it is recursive or a
            // specialization for that function exists: then the call goes to the callee's
            // specialization ([`specialization`]).
            let passed = self.passed_functions(arguments);
            let existing = passed
                .as_ref()
                .ok()
                .and_then(|passing| existing_specialization(function, &passing.passed));
            if existing.is_none()
                && function.tag() == self.env
                && function != self.closure
                && !self.inlining.contains(&function)
                && !self.called.contains(&function)
            {
                self.inlining.push(function);
                let value = self.inline(function.car(), body, 0, arguments, crate::storage);
                self.inlining.pop();
                // A refusal that widens a variable's tags lowers again inlined; any other goes
                // to the callee's specialization.
                if value.is_err() && self.widen.is_none() {
                    self.refused.get_or_insert(function);
                }
                return value;
            }
            if function.tag() != self.env {
                return Err(Refusal::new(
                    "a function passed to a typed function of another environment",
                ));
            }
            let Passing {
                passed,
                captured,
                globals,
            } = passed?;
            for symbol in globals {
                self.read_before_assignment(symbol)?;
            }
            let synthetic = match existing {
                Some(synthetic) => synthetic,
                None => specialization(function, &passed)?,
            };
            let mut values: SmallVec<[Local; 4]> = SmallVec::new();
            for &argument in arguments {
                if let Argument::Value(value) = argument {
                    values.push(value);
                }
            }
            // A name set on some paths only gives the call up where it is unset, before the
            // call rather than where the literal reads it, and the interpreter runs the call.
            for value in &captured {
                if value.maybe {
                    let flag = self.flag(value.local)?;
                    let (set, unset) = (self.builder.block(), self.builder.block());
                    self.builder.terminate(Terminator::Branch {
                        condition: flag,
                        then: set,
                        otherwise: unset,
                    });
                    self.builder.switch_to(set);
                }
                values.push(value.local);
            }
            let result = self.typed_call(synthetic, &values)?;
            // The passed function reads a captured parameter somewhere inside the call, so the
            // read counts after it, where R may already have run code the call makes.
            let block = self.builder.current;
            let position = self
                .builder
                .function
                .blocks
                .get(block.0 as usize)
                .map_or(0, |block| block.insts.len()) as u32;
            for value in &captured {
                for param in self.forced_by(value.local) {
                    self.param_reads.push(ParamRead {
                        block,
                        position,
                        param,
                        into: None,
                    });
                }
            }
            return Ok(Some(result));
        };
        let values = &values[..];
        let own = function == self.closure;
        let mut own_calls = if own { MAX_SELF_CALLS } else { 0 };
        if function.tag() == self.env
            && (!own || self.self_inlines < MAX_SELF_CALLS)
            && !self.inlining.contains(&function)
            && !self.called.contains(&function)
            && self.inlined.iter().filter(|inlined| inlined.typed).count() < MAX_TYPED_INLINE_DEPTH
            && rvalues::pairlist_items(function.car()).len() == values.len()
            && small_body(body, name, &mut own_calls, &mut 0)
        {
            self.self_inlines += own as usize;
            self.inlining.push(function);
            let chain = std::mem::replace(&mut self.tail_chain, tail);
            let value = self.inline(function.car(), body, 0, arguments, crate::storage);
            self.tail_chain = chain;
            self.inlining.pop();
            if value.is_err() {
                self.refused.get_or_insert(function);
            }
            return value;
        }
        let value = self.typed_call(function, values)?;
        if tail {
            self.builder.function.tail_calls.push(value);
        }
        Ok(Some(value))
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

    /// The functions `arguments` pass, by formal position, with the values the passed literals
    /// and local functions read from the scopes they see, in the order their names list them
    /// ([`PassedFunction::Literal`]), and the names they read that no scope binds.
    unsafe fn passed_functions(&self, arguments: &[Argument]) -> Result<Passing, Refusal> {
        let mut passing = Passing::default();
        for (position, &argument) in arguments.iter().enumerate() {
            if let Argument::Function(callable) = argument {
                self.describe_passed(PassedAt::Formal(position), callable, &mut passing)?;
            }
        }
        Ok(passing)
    }

    /// Adds `callable` to `passing` at `at`, then each function it reads by name, a literal read
    /// by a literal it wraps being refused so that a recursion cannot specialize without end.
    unsafe fn describe_passed(
        &self,
        at: PassedAt,
        callable: Callable,
        passing: &mut Passing,
    ) -> Result<(), Refusal> {
        let (formals, body, capture) = match callable {
            Callable::Typed(name, function) => {
                passing
                    .passed
                    .push((at, PassedFunction::Typed(name, function), SmallVec::new()));
                return Ok(());
            }
            Callable::Library(name) => {
                passing
                    .passed
                    .push((at, PassedFunction::Library(name), SmallVec::new()));
                return Ok(());
            }
            Callable::Inline(formals, body, scope) => (formals, body, Err(scope)),
            Callable::Captured(formals, body, capture) => (formals, body, Ok(capture)),
        };
        let literal = PassedFunction::Literal(formals, body);
        let mut depth = 0;
        let mut ancestor = at;
        while let PassedAt::Read { owner, .. } = ancestor {
            depth += 1;
            let Some(&(owner_at, owner_function, _)) = passing.passed.get(owner) else {
                break;
            };
            if owner_function == literal || depth >= MAX_PASSED_DEPTH {
                return Err(Refusal::new(
                    "a function literal passed to a recursive typed function that wraps the \
                     function its caller was passed",
                ));
            }
            ancestor = owner_at;
        }
        let sees = |symbol: Sexp| match capture {
            Err(scope) => self
                .scopes
                .iter()
                .take(scope + 1)
                .rev()
                .find_map(|held| held.names.get(&symbol).copied()),
            Ok(capture) => self
                .captures
                .get(capture as usize)
                .and_then(|names| names.get(&symbol).copied()),
        };
        let own: SmallVec<[Sexp; 4]> = rvalues::pairlist_items(formals)
            .iter()
            .map(|&(symbol, _)| symbol)
            .collect();
        let mut symbols = SmallVec::new();
        free_symbols(formals, &mut symbols);
        free_symbols(body, &mut symbols);
        let index = passing.passed.len();
        passing.passed.push((at, literal, SmallVec::new()));
        let mut functions: SmallVec<[(Sexp, Callable); 2]> = SmallVec::new();
        for symbol in symbols {
            if own.contains(&symbol) {
                continue;
            }
            let (local, maybe) = match sees(symbol) {
                None => {
                    if !passing.globals.contains(&symbol) {
                        passing.globals.push(symbol);
                    }
                    continue;
                }
                Some(Binding::Function(callable)) => {
                    functions.push((symbol, callable));
                    continue;
                }
                Some(Binding::Hidden) => return Err(hidden_refusal()),
                Some(Binding::Value(local)) => (local, false),
                Some(Binding::Maybe(local)) => (local, true),
            };
            if self.unusable.contains(&local)
                || self.pending.iter().any(|held| held.contains(&local))
            {
                return Err(Refusal::new(
                    "a function literal passed to a recursive typed function that reads an \
                     empty list or a value its caller has given away",
                ));
            }
            if let Some((.., names)) = passing.passed.get_mut(index) {
                names.push(symbol);
            }
            passing.captured.push(CapturedValue { local, maybe });
        }
        for (name, callable) in functions {
            self.describe_passed(PassedAt::Read { owner: index, name }, callable, passing)?;
        }
        Ok(())
    }

    /// The arguments of a call of a local or typed function. A function literal, a local
    /// function and a name that holds a function pass that function, and any other argument
    /// passes its value.
    unsafe fn passed_arguments(
        &mut self,
        arguments: &Arguments,
        spans: &mut SmallVec<[ArgumentReads; 4]>,
    ) -> Result<SmallVec<[Argument; 4]>, Refusal> {
        let mut passed = SmallVec::new();
        for &(_, argument) in &arguments.items {
            let start = self.argument_start();
            passed.push(if self.passes_function(argument) {
                Argument::Function(self.function_argument(argument)?)
            } else {
                Argument::Value(self.value(argument)?)
            });
            spans.push(self.argument_reads(start, false));
        }
        Ok(passed)
    }

    /// The arguments of a call of a function with `formals`, one per formal in their order and
    /// matched as R matches them ([`match_formals`]), computed in the order they are written. A
    /// formal the call leaves out takes its default, lowered as the callee's `frame` computes it.
    /// `spans` gets the parameter reads each argument made, by formal.
    unsafe fn matched_arguments(
        &mut self,
        formals: Sexp,
        arguments: &Arguments,
        frame: Frame,
        spans: &mut SmallVec<[ArgumentReads; 4]>,
    ) -> Result<SmallVec<[Argument; 4]>, Refusal> {
        let formals = rvalues::pairlist_items(formals);
        let supplied = match_formals(&formals, &arguments.items)?;
        let mut written: SmallVec<[(Option<Argument>, ArgumentReads); 4]> = SmallVec::new();
        for &(_, code) in &arguments.items {
            let start = self.argument_start();
            let argument = if code == rvalues::missing_arg() {
                None
            } else if self.passes_function(code) {
                Some(Argument::Function(self.function_argument(code)?))
            } else {
                Some(Argument::Value(self.value(code)?))
            };
            written.push((argument, self.argument_reads(start, false)));
        }
        let mut matched: SmallVec<[Option<Argument>; 4]> = SmallVec::new();
        spans.clear();
        for &item in &supplied {
            let (argument, span) = item.map_or((None, NO_ARGUMENT_READS), |item| written[item]);
            matched.push(argument);
            spans.push(span);
        }
        if matched.iter().any(Option::is_none) {
            self.frame_defaults(&formals, &mut matched, spans, frame)?;
        }
        Ok(matched.into_iter().flatten().collect())
    }

    /// The arguments of a call that passes the first formals of a function by position, as a
    /// standard library function calls the function passed to it, with the formals past them
    /// taking their defaults as a direct call's do ([`Lowering::matched_arguments`]). `visible`
    /// counts the scopes the function sees.
    unsafe fn positional_arguments(
        &mut self,
        formals: Sexp,
        body: Sexp,
        visible: usize,
        arguments: &[Argument],
    ) -> Result<SmallVec<[Argument; 4]>, Refusal> {
        let items = rvalues::pairlist_items(formals);
        let supplied = arguments.len();
        if supplied >= items.len() {
            return Ok(arguments.iter().copied().collect());
        }
        let mut matched: SmallVec<[Option<Argument>; 4]> = arguments
            .iter()
            .copied()
            .map(Some)
            .chain((supplied..items.len()).map(|_| None))
            .collect();
        let mut spans: SmallVec<[ArgumentReads; 4]> =
            (0..items.len()).map(|_| NO_ARGUMENT_READS).collect();
        // The passed arguments keep the reads their call attributed; the defaults' reads count
        // where the body first reads their formals, as a direct call's do.
        let held = std::mem::take(&mut self.formal_reads);
        let strict = self.inlined.last().is_none_or(|inlined| inlined.typed)
            && typed_definition(formals, body);
        let frame = Frame::Local(visible, body);
        let values = self
            .frame_defaults(&items, &mut matched, &mut spans, frame)
            .and_then(|()| {
                let values: SmallVec<[Argument; 4]> = matched.into_iter().flatten().collect();
                self.attribute_reads(&values[supplied..], &spans[supplied..], strict)?;
                Ok(values)
            });
        // A refused call leaves no reads behind for the next inlined function to take.
        let defaults = std::mem::take(&mut self.formal_reads);
        let values = values?;
        self.formal_reads = held;
        self.formal_reads.resize(supplied, SmallVec::new());
        self.formal_reads.extend(defaults);
        Ok(values)
    }

    /// Computes the defaults of the formals `matched` leaves out, in the callee's `frame`.
    unsafe fn frame_defaults(
        &mut self,
        formals: &[(Sexp, Sexp)],
        matched: &mut [Option<Argument>],
        spans: &mut [ArgumentReads],
        frame: Frame,
    ) -> Result<(), Refusal> {
        // A typed callee's defaults read names of its own body and environment only.
        let (visible, body, env, closure) = match frame {
            Frame::Local(visible, body) => (visible, body, self.env, self.closure),
            Frame::Typed(function) => (
                0,
                interp::bytecode::closure_body(function),
                function.tag(),
                function,
            ),
        };
        let hidden = self.scopes.split_off(visible.min(self.scopes.len()));
        let loops = std::mem::take(&mut self.loops);
        let control = std::mem::replace(&mut self.control, 0);
        let env = std::mem::replace(&mut self.env, env);
        let closure = std::mem::replace(&mut self.closure, closure);
        let inlining = match frame {
            Frame::Local(..) => self.inlining.clone(),
            Frame::Typed(_) => SmallVec::new(),
        };
        let inlining = std::mem::replace(&mut self.inlining, inlining);
        let mut scope = self.new_scope();
        for (&(symbol, _), argument) in formals.iter().zip(matched.iter()) {
            // A default sees a formal passed a function only by calling it, which no default
            // in typed code needs, and sees a left-out formal once its own default is in.
            let binding = match *argument {
                Some(Argument::Value(value)) => Binding::Value(value),
                _ => Binding::Hidden,
            };
            scope.names.insert(symbol, binding);
        }
        self.scopes.push(scope);
        let defaults = self.defaults(formals, matched, spans, body);
        self.scopes.pop();
        self.scopes.extend(hidden);
        self.inlining = inlining;
        self.closure = closure;
        self.env = env;
        self.loops = loops;
        self.control = control;
        defaults
    }

    /// Binds the formals a call from R code leaves out, past the parameters or standing for an
    /// [`Type::Absent`] one, to their defaults. R computes a default where the body first reads
    /// its formal, so the parameters a default reads count as read there
    /// (`Lowering::param_aliases`), as an untyped local function's argument does.
    unsafe fn left_out_defaults(
        &mut self,
        formals: &[(Sexp, Sexp)],
        assigned: &[Sexp],
    ) -> Result<SmallVec<[Type; 2]>, Refusal> {
        let left_out: SmallVec<[usize; 4]> = (0..formals.len())
            .filter(|&index| self.params.get(index).is_none_or(|&ty| ty == Type::Absent))
            .collect();
        let mut matched: SmallVec<[Option<Argument>; 4]> = (0..formals.len())
            .map(|index| {
                self.param_variables
                    .get(index)
                    .filter(|_| !left_out.contains(&index))
                    .map(|&variable| Argument::Value(variable))
            })
            .collect();
        let mut spans: SmallVec<[ArgumentReads; 4]> =
            (0..formals.len()).map(|_| NO_ARGUMENT_READS).collect();
        self.defaults(formals, &mut matched, &mut spans, self.body)?;
        let values: SmallVec<[Argument; 4]> = left_out
            .iter()
            .filter_map(|&index| matched.get(index).copied().flatten())
            .collect();
        let left_out_spans: SmallVec<[ArgumentReads; 4]> = left_out
            .iter()
            .filter_map(|&index| spans.get(index).copied())
            .collect();
        self.attribute_reads(&values, &left_out_spans, false)?;
        let formal_reads = std::mem::take(&mut self.formal_reads);
        let left_out_formals = left_out.iter().filter_map(|&index| formals.get(index));
        let mut types = SmallVec::new();
        for (offset, (&(symbol, _), value)) in left_out_formals.zip(&values).enumerate() {
            let Argument::Value(value) = *value else {
                return Err(Refusal::new("a default that is a function"));
            };
            let ty = self.builder.ty(value);
            types.push(ty);
            let variable = if assigned.contains(&symbol) {
                self.builder.emit(ty, Op::Copy(value))
            } else {
                value
            };
            if let Some(reads) = formal_reads.get(offset).filter(|reads| !reads.is_empty()) {
                self.param_aliases.push((variable, reads.clone()));
            }
            self.bind(symbol, Binding::Value(variable));
        }
        Ok(types)
    }

    /// The defaults of the formals `matched` leaves out of a call of the function whose body is
    /// `body`. R computes a default when the body first reads it, and native code computes it at
    /// the call, so a default compiles only when that cannot change its value or what the call
    /// does ([`Lowering::computed_ahead`]), and one that reads another left-out formal comes
    /// after that formal's.
    unsafe fn defaults(
        &mut self,
        formals: &[(Sexp, Sexp)],
        matched: &mut [Option<Argument>],
        spans: &mut [ArgumentReads],
        body: Sexp,
    ) -> Result<(), Refusal> {
        loop {
            let waiting = |matched: &[Option<Argument>], default: Sexp| {
                formals.iter().zip(matched).any(|(&(symbol, _), argument)| {
                    argument.is_none() && mentions_outside(default, rvalues::nil(), symbol)
                })
            };
            let next = formals
                .iter()
                .zip(matched.iter())
                .position(|(&(_, default), argument)| {
                    argument.is_none() && !waiting(matched, default)
                });
            let Some(index) = next else {
                return match matched.iter().position(Option::is_none) {
                    Some(index) => Err(Refusal(Cow::Owned(format!(
                        "the default of `{}` reads a formal whose default reads it in turn",
                        rvalues::symbol_name(formals[index].0)
                    )))),
                    None => Ok(()),
                };
            };
            let (symbol, default) = formals[index];
            let name = || rvalues::symbol_name(symbol);
            if default == rvalues::missing_arg() {
                return Err(Refusal(Cow::Owned(format!(
                    "the call leaves out `{}`, which has no default",
                    name()
                ))));
            }
            if !self.computed_ahead(default, body) {
                return Err(Refusal(Cow::Owned(format!(
                    "the default of `{}` is not made of constants, operators, the standard \
                     library and parameters or local variables the body does not set",
                    name()
                ))));
            }
            let start = self.argument_start();
            let value = self.value(default)?;
            matched[index] = Some(Argument::Value(value));
            if let Some(span) = spans.get_mut(index) {
                *span = self.argument_reads(start, true);
            }
            self.bind(symbol, Binding::Value(value));
        }
    }

    /// Whether the default `code` of a function whose body is `body` has the same value and
    /// effect computed before the body runs as where the body first reads it: it is made of
    /// constants, values of the frame the body does not set, operators, ry literals and calls of
    /// the standard library. A global may change under any R call the body makes, so a default
    /// reads none but the library's constants. The checker judges defaults the same way
    /// (`computed_ahead` in `semantics::native`).
    unsafe fn computed_ahead(&self, code: Sexp, body: Sexp) -> bool {
        match code.kind() {
            // A name set on some paths only reads as the interpreter does where it is set, and
            // gives the call up where it is not.
            SexpType::Symbol => match self.lookup(code) {
                Some(Binding::Value(_) | Binding::Maybe(_)) => !assigns(body, code),
                Some(Binding::Function(_) | Binding::Hidden) => false,
                None => {
                    !assigns(body, code)
                        && std_binding(code, self.env).is_some_and(|value| {
                            !matches!(
                                value.kind(),
                                SexpType::Closure | SexpType::Builtin | SexpType::Special
                            )
                        })
                }
            },
            // The body's own definitions are not in scope at the call, so a head it binds
            // (`` `+` <- ``, `Num.abs <- `) would call another function where R reads the default.
            SexpType::Language => {
                let head = code.car();
                head.kind() == SexpType::Symbol
                    && self.lookup(head).is_none()
                    && !assigns(body, head)
                    && (is_ahead_operator(&rvalues::symbol_name(head))
                        || std_binding(head, self.env).is_some())
                    && rvalues::pairlist_items(code.cdr())
                        .iter()
                        .all(|&(_, item)| self.computed_ahead(item, body))
            }
            _ => true,
        }
    }

    /// Whether the argument `code` is a function: a function literal, a local function, or a
    /// name whose value lookup finds a function.
    unsafe fn passes_function(&self, code: Sexp) -> bool {
        match code.kind() {
            SexpType::Symbol => match self.lookup(code) {
                Some(binding) => matches!(binding, Binding::Function(_)),
                None => find_function(code, self.env).is_some(),
            },
            SexpType::Language => {
                code.car().kind() == SexpType::Symbol
                    && rvalues::symbol_name(code.car()) == "function"
                    && self.lookup(code.car()).is_none()
            }
            _ => false,
        }
    }

    /// `callable` as a function that sees the names it sees here wherever it is called: a local
    /// function or a literal takes the bindings of the scopes it sees along, since the function
    /// it is passed to lowers with other scopes in view.
    fn captured(&mut self, callable: Callable) -> Callable {
        match callable {
            Callable::Inline(formals, body, scope) => {
                Callable::Captured(formals, body, self.capture(scope, &mut SmallVec::new()))
            }
            other => other,
        }
    }

    /// The bindings of the first `scope + 1` scopes as one capture, an inner scope's binding of
    /// a name hiding an outer one's. The local functions among them are captured in turn, each
    /// with the scopes it sees, which a function defined in this scope shares.
    fn capture(&mut self, scope: usize, shared: &mut SmallVec<[(usize, u32); 4]>) -> u32 {
        if let Some(&(_, id)) = shared.iter().find(|&&(held, _)| held == scope) {
            return id;
        }
        let id = self.captures.len() as u32;
        self.captures.push(FxHashMap::default());
        shared.push((scope, id));
        let mut names: FxHashMap<Sexp, Binding> = FxHashMap::default();
        for held in self.scopes.iter().take(scope + 1) {
            names.extend(
                held.names
                    .iter()
                    .map(|(&symbol, &binding)| (symbol, binding)),
            );
        }
        for binding in names.values_mut() {
            if let Binding::Function(Callable::Inline(formals, body, defined)) = *binding {
                let capture = self.capture(defined, shared);
                *binding = Binding::Function(Callable::Captured(formals, body, capture));
            }
        }
        self.captures[id as usize] = names;
        id
    }

    unsafe fn arguments(&self, call: Sexp, count: usize) -> Result<SmallVec<[Sexp; 4]>, Refusal> {
        Arguments::of(call).exactly(count)
    }

    /// Leaves the innermost inlined function, or the function itself, with `value`.
    unsafe fn exit(&mut self, value: Local) -> Result<(), Refusal> {
        match self.inlined.last() {
            Some(inlined) => {
                let (exit, returns_i64) = (inlined.exit, inlined.returns_i64);
                self.usable(value)?;
                let value = if returns_i64 {
                    self.to_i64(value)?
                } else {
                    value
                };
                let block = self.builder.current;
                if let Some(inlined) = self.inlined.last_mut() {
                    inlined.early.push((block, value));
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

    /// Lowers `expression`, whose value the program drops when `dropped` is true, so that it may be
    /// a call of an R function whose result nothing converts.
    unsafe fn part(&mut self, expression: Sexp, dropped: bool) -> Result<Option<Local>, Refusal> {
        if !dropped {
            return self.expression(expression);
        }
        self.r_use = Some(RUse::Unused);
        let value = self.expression(expression);
        self.r_use = None;
        value
    }

    /// `dropped` says the program drops the block's value, and with it the last statement's.
    unsafe fn sequence(&mut self, call: Sexp, dropped: bool) -> Result<Option<Local>, Refusal> {
        let items = rvalues::pairlist_items(call.cdr());
        let mut last = None;
        if items.is_empty() {
            return Ok(Some(
                self.builder.emit(Type::Null, Op::Const(Constant::Null)),
            ));
        }
        let count = items.len();
        for (position, (_, item)) in items.into_iter().enumerate() {
            let dropped = dropped || position + 1 < count;
            last = if self.probing > 0 {
                self.trial_part(item, dropped)?
            } else {
                self.part(item, dropped)?
            };
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
                // A branch that rebinds a name leaves the old binding on the paths that skip it,
                // which a local function, inlined where it is called, cannot express. A name
                // another branch hid is unusable after the branches either way, so it may be
                // defined again, as when both branches of an `if` define it.
                if self.control > 0
                    && self.scopes.last().is_some_and(|scope| {
                        scope
                            .names
                            .get(&target)
                            .is_some_and(|binding| !matches!(binding, Binding::Hidden))
                    })
                {
                    return Err(Refusal::new("a local function rebound in a branch or loop"));
                }
                let scope = self.scopes.len() - 1;
                self.bind(
                    target,
                    Binding::Function(Callable::Inline(formals, body, scope)),
                );
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
            | (Type::Lgl | Type::Bool, Item::Lgl)
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

    /// `dropped` says the program drops the `if`'s value, and with it its branches'.
    unsafe fn conditional(&mut self, call: Sexp, dropped: bool) -> Result<Option<Local>, Refusal> {
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
        let region = self.enter_control();
        let then_value = self.part(then, dropped)?;
        if let Some(value) = then_value
            && self.inlined.is_empty()
            && self.tails.contains(&call)
        {
            self.tail_branch.get_or_insert(self.builder.ty(value));
        }
        let then_created = self.leave_control(region);
        let then_end = self.builder.current;
        self.builder.switch_to(else_block);
        let region = self.enter_control();
        let else_value = match otherwise {
            Some(otherwise) => self.part(otherwise, dropped)?,
            None => Some(self.builder.emit(Type::Null, Op::Const(Constant::Null))),
        };
        let else_created = self.leave_control(region);
        let else_end = self.builder.current;
        let ends: SmallVec<[(BlockId, Created); 2]> = [
            then_value.map(|_| (then_end, then_created)),
            else_value.map(|_| (else_end, else_created)),
        ]
        .into_iter()
        .flatten()
        .collect();
        self.join_names(&ends);
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

    /// Opens a loop over the positions of `held`, a list the caller holds for the whole loop,
    /// and leaves the builder in the loop's body.
    fn positions(&mut self, held: Local) -> Positions {
        let count = self.builder.emit(Type::I64, Op::Length(held));
        let one = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
        self.count_up(one, count)
    }

    /// Opens a loop whose `I64` counter runs from `start` to `end`, with no iteration when `end`
    /// is below `start`, and leaves the builder in the loop's body. The loop leaves once the
    /// counter has reached `end`, so the step never passes it and cannot overflow.
    fn count_up(&mut self, start: Local, end: Local) -> Positions {
        let counter = self.builder.emit(Type::I64, Op::Copy(start));
        // The bound is read once: `end` may be a variable the body assigns.
        let end = self.builder.emit(Type::I64, Op::Copy(end));
        let empty = self
            .builder
            .emit(Type::Bool, Op::Compare(Compare::Lt, Item::I64, end, start));
        let (body, exit) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: empty,
            then: exit,
            otherwise: body,
        });
        self.builder.switch_to(body);
        let header = self.loop_header();
        Positions {
            counter,
            end,
            header,
            exit,
        }
    }

    /// Closes the body of a `count_up` loop: steps the counter unless it has reached the end,
    /// and continues after the loop.
    fn next_position(&mut self, positions: Positions) {
        let Positions {
            counter,
            end,
            header,
            exit,
        } = positions;
        let done = self.builder.emit(Type::Bool, Op::Same(counter, end));
        let step = self.builder.block();
        self.builder.terminate(Terminator::Branch {
            condition: done,
            then: exit,
            otherwise: step,
        });
        self.builder.switch_to(step);
        let one = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
        let next = self.builder.emit(Type::I64, Op::Step(counter, one));
        self.builder.push(Some(counter), Op::Copy(next));
        self.builder.terminate(Terminator::Jump(header));
        self.builder.switch_to(exit);
    }

    /// Opens the block a loop jumps back to, which checks for an interrupt first.
    fn loop_header(&mut self) -> BlockId {
        let header = self.builder.block();
        self.builder.terminate(Terminator::Jump(header));
        self.builder.switch_to(header);
        self.builder.push(None, Op::Poll);
        header
    }

    /// Lowers the loop `call` with `lower`. A read in the loop of a name that only a later
    /// statement of its body sets refuses at first, because the name has no variable yet. The
    /// loop is then lowered as a trial that leaves out the statements reading such a name, which
    /// gives the name's type, and lowered again with the name declared before it as a name not
    /// set yet: the read gives up where the name is unset, and the interpreter reads it as R does.
    unsafe fn looped(
        &mut self,
        call: Sexp,
        lower: unsafe fn(&mut Self, Sexp) -> Result<Option<Local>, Refusal>,
    ) -> Result<Option<Local>, Refusal> {
        let checkpoint = self.checkpoint();
        let start = self.builder.current;
        let start_insts = self
            .builder
            .function
            .blocks
            .get(start.0 as usize)
            .map_or(0, |block| block.insts.len());
        let scopes = self.scopes.len();
        // Only a loop at the function's own level lowers in one context: an inlined function's
        // loop takes other types where it inlines again.
        let remembers = scopes == 1;
        let mut declared: SmallVec<[Declared; 2]> = self
            .declared_loops
            .iter()
            .find(|&&(loop_call, _)| remembers && loop_call == call)
            .map(|(_, names)| names.clone())
            .unwrap_or_default();
        self.declare(&declared);
        loop {
            self.forward = None;
            let refusal = match lower(self, call) {
                Ok(value) => {
                    if remembers && self.probing == 0 && !declared.is_empty() {
                        self.declared_loops
                            .retain(|&(loop_call, _)| loop_call != call);
                        self.declared_loops.push((call, declared));
                    }
                    return Ok(value);
                }
                Err(refusal) => refusal,
            };
            let Some((symbol, at)) = self.forward else {
                return Err(refusal);
            };
            // A name only an enclosing loop's body sets is that loop's to declare.
            if !assigns(call, symbol) {
                return Err(refusal);
            }
            self.forward = None;
            if at != scopes
                || declared.iter().any(|&(name, ..)| name == symbol)
                || !self.restart(checkpoint.clone(), start, start_insts)
            {
                return Err(refusal);
            }
            self.declare(&declared);
            let refused = *self.refused;
            self.probing += 1;
            let trial = lower(self, call);
            self.probing -= 1;
            *self.refused = refused;
            let learned = match (trial, self.lookup(symbol)) {
                (Ok(_), Some(Binding::Maybe(local) | Binding::Value(local))) => (
                    symbol,
                    self.builder.ty(local),
                    self.pending_family(local).is_some(),
                ),
                _ => return Err(refusal),
            };
            if !self.restart(checkpoint.clone(), start, start_insts) {
                return Err(refusal);
            }
            declared.push(learned);
            self.declare(&declared);
        }
    }

    /// Binds each name of `declared` to a variable of its type that is not set yet. An empty
    /// list of records or tags the trial left pending is pending in its variable too.
    fn declare(&mut self, declared: &[Declared]) {
        for &(symbol, ty, pending) in declared {
            let local = self.variable(symbol, ty);
            if pending {
                self.pending.push(SmallVec::from_slice(&[local]));
            }
            self.bind(symbol, Binding::Maybe(local));
        }
    }

    /// Lowers `expression`, a statement of a loop trial ([`Lowering::looped`]), leaving it out
    /// when it reads a name that only a later statement sets.
    unsafe fn trial_part(
        &mut self,
        expression: Sexp,
        dropped: bool,
    ) -> Result<Option<Local>, Refusal> {
        let checkpoint = self.checkpoint();
        let start = self.builder.current;
        let start_insts = self
            .builder
            .function
            .blocks
            .get(start.0 as usize)
            .map_or(0, |block| block.insts.len());
        self.forward = None;
        match self.part(expression, dropped) {
            Err(refusal) if self.forward.is_some() => {
                if !self.restart(checkpoint, start, start_insts) {
                    return Err(refusal);
                }
                Ok(Some(
                    self.builder.emit(Type::Null, Op::Const(Constant::Null)),
                ))
            }
            value => value,
        }
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
                let one = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
                let length = self.builder.emit(Type::I64, Op::Length(held));
                (one, length, Step::FromOne(one), Some(held))
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
                let zero = match counter_ty {
                    Type::I64 => self.builder.emit(Type::I64, Op::Const(Constant::I64(0))),
                    _ => self.int(0),
                };
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
        let region = self.enter_control();
        let item = match items {
            Some(held) => self.builder.emit(
                Type::of_item(
                    self.builder
                        .ty(held)
                        .items()
                        .ok_or(Refusal::new("a loop over a non-vector"))?,
                ),
                Op::ItemAt(held, counter),
            ),
            None => counter,
        };
        self.assign(variable, item)?;
        let latch = self.builder.block();
        self.loops.push((latch, exit));
        let completed = self.part(body, true)?;
        self.loops.pop();
        let created = self.leave_control(region);
        self.leave_loop(created);
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
        let Some(speculation) = find_function(head, self.env) else {
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
        let condition_expression = condition;
        // The condition runs again on every pass, so a name bound before the loop keeps its type
        // there as in the body. Names the condition creates stay bound after it, since it always
        // runs.
        self.control += 1;
        let names = self
            .scopes
            .last()
            .map(|scope| scope.names.clone())
            .unwrap_or_default();
        self.regions.push(names);
        let condition = self.condition(condition);
        self.regions.pop();
        self.control -= 1;
        let condition = condition?;
        let (body_block, exit) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition,
            then: body_block,
            otherwise: exit,
        });
        self.builder.switch_to(body_block);
        self.loops.push((header, exit));
        let region = self.enter_control();
        let completed = self.part(body, true)?;
        let created = self.leave_control(region);
        self.leave_loop(created);
        self.loops.pop();
        if completed.is_some() {
            self.builder.terminate(Terminator::Jump(header));
        }
        self.builder.switch_to(exit);
        // A `while (TRUE)` that only a `return` leaves gives no value, like a `repeat`. Its exit
        // block keeps the `Fail` a new block ends in, which never runs.
        if is_true(condition_expression) && !breaks(body) {
            return Ok(None);
        }
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
        let region = self.enter_control();
        let completed = self.part(body, true)?;
        let created = self.leave_control(region);
        self.leave_loop(created);
        self.loops.pop();
        if completed.is_some() {
            self.builder.terminate(Terminator::Jump(header));
        }
        self.builder.switch_to(exit);
        // Without a `break`, only a `return` leaves the loop, so it gives no value.
        if !breaks(body) {
            return Ok(None);
        }
        Ok(Some(
            self.builder.emit(Type::Null, Op::Const(Constant::Null)),
        ))
    }

    /// Inlines a function called with `arguments`: a local function, a function literal, or a
    /// small typed function defined beside this one. `storage` gives the R storage type the
    /// function's annotations check each value as, which is the one a separate call of that
    /// function would check.
    unsafe fn inline(
        &mut self,
        formals: Sexp,
        body: Sexp,
        visible: usize,
        arguments: &[Argument],
        storage: fn(Type) -> Option<(SexpType, bool)>,
    ) -> Result<Option<Local>, Refusal> {
        let formal_reads = std::mem::take(&mut self.formal_reads);
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
        let mut converted: SmallVec<[Argument; 4]> = SmallVec::new();
        for (index, &argument) in arguments.iter().enumerate() {
            converted.push(match argument {
                Argument::Value(value) if i64_formals.get(index).copied().unwrap_or(false) => {
                    Argument::Value(self.to_i64(value)?)
                }
                // The body runs with other scopes in view, so a passed function brings the names
                // it sees with it.
                Argument::Function(callable) => Argument::Function(self.captured(callable)),
                argument => argument,
            });
        }
        let arguments = &converted[..];
        let mut scope = self.new_scope();
        for (index, (&(symbol, _), &argument)) in formals.iter().zip(arguments).enumerate() {
            if symbol == rvalues::symbols().dots {
                return Err(Refusal::new("a local function with `...`"));
            }
            let argument = match argument {
                Argument::Value(value) => value,
                Argument::Function(callable) => {
                    scope.names.insert(symbol, Binding::Function(callable));
                    continue;
                }
            };
            // A parameter is a fresh variable, so assigning it never touches the caller's, and
            // one holding a tag widens as any variable does.
            let ty = self.widened_type(symbol, self.builder.ty(argument));
            let variable = self.builder.local(ty);
            self.builder.push(Some(variable), Op::Copy(argument));
            if let Some(params) = formal_reads.get(index)
                && !params.is_empty()
            {
                self.param_aliases.push((variable, params.clone()));
            }
            scope.names.insert(symbol, Binding::Value(variable));
        }
        // The body sees the names of the scope that defines the function, as R's lexical scoping
        // does, not those of the inlined functions that call it: the first `visible` scopes,
        // which is none of them for a function defined outside this one. A `break` or `next` in
        // it does not reach the caller's loops, and its own branches start at its own top level.
        let hidden = self.scopes.split_off(visible.min(self.scopes.len()));
        let loops = std::mem::take(&mut self.loops);
        let control = std::mem::replace(&mut self.control, 0);
        self.scopes.push(scope);
        let exit = self.builder.block();
        // A `return` or `try` before the end jumps to `exit` with its value, and the result's
        // type is the join of those values and the body's.
        self.inlined.push(Inlined {
            exit,
            returns_i64,
            early: SmallVec::new(),
            typed: typed_definition(definition, body),
        });
        let value = match self.expression(body) {
            Ok(Some(value)) => self.usable(value).and_then(|()| {
                if returns_i64 {
                    self.to_i64(value).map(Some)
                } else {
                    Ok(Some(value))
                }
            }),
            other => other,
        };
        let mut ends = self
            .inlined
            .pop()
            .map(|inlined| inlined.early)
            .unwrap_or_default();
        self.scopes.pop();
        self.scopes.extend(hidden);
        self.loops = loops;
        self.control = control;
        if let Some(value) = value? {
            ends.push((self.builder.current, value));
        }
        let mut ty: Option<Type> = None;
        for &(_, value) in &ends {
            let found = self.builder.ty(value);
            ty = Some(match ty {
                None => found,
                Some(held) => {
                    joined(held, found).ok_or(Refusal::new("a `return` of another type"))?
                }
            });
        }
        let Some(ty) = ty else {
            self.builder.switch_to(exit);
            self.builder.terminate(Terminator::Fail);
            return Ok(None);
        };
        let kinds: SmallVec<[Option<(SexpType, bool)>; 4]> = arguments
            .iter()
            .map(|&argument| match argument {
                Argument::Value(value) => storage(self.builder.ty(value)),
                Argument::Function(_) => Some((SexpType::Closure, false)),
            })
            .collect();
        if !interp::annotations::admits_inline(definition, body, &kinds, storage(ty)) {
            return Err(Refusal::new(
                "a local function whose annotations reject its types",
            ));
        }
        let result = self.builder.local(ty);
        for &(end, value) in &ends {
            // An early exit's block already jumps to `exit`; the copy goes before the jump.
            self.builder.switch_to(end);
            self.builder.push(Some(result), Op::Copy(value));
            self.builder.terminate(Terminator::Jump(exit));
        }
        self.builder.switch_to(exit);
        Ok(Some(result))
    }

    fn int(&mut self, value: i32) -> Local {
        self.builder
            .emit(Type::Int, Op::Const(Constant::Int(value)))
    }

    fn shape_id(&mut self, shape: u32) -> Local {
        self.builder
            .emit(Type::Int, Op::Const(Constant::Shape(shape)))
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
        // An operator with a table expression among its operands builds an expression, which
        // R's own operator does through the expression's method.
        if is_operator(name) && arguments.items.iter().any(|&(_, code)| self.is_table(code)) {
            return self.r_call(call, function, RUse::Table);
        }
        // Where the call may be one of R, an argument only R can evaluate makes it one.
        if let Some(r_use) = r_use
            && takes_r_arguments(name)
            && arguments
                .items
                .iter()
                .any(|&(_, code)| self.only_r_evaluates(code))
        {
            return self.r_call(call, function, r_use);
        }
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
                let region = self.enter_control();
                let b = self.condition(b)?;
                self.leave_control(region);
                let b = self.builder.emit(Type::Lgl, Op::Copy(b));
                self.builder.function.locals[b.0 as usize] = Type::Lgl;
                self.builder.push(Some(result), Op::Copy(b));
                self.builder.terminate(Terminator::Jump(join));
                self.builder.switch_to(join);
                Ok(result)
            }
            "Set.from_list" => self.set_from_list(&arguments),
            "Set.single" => {
                let [element] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `Set.single`"));
                };
                let element = self.value(element)?;
                let item = match self.builder.ty(element) {
                    Type::I64 => Item::I64,
                    Type::Str => Item::Str,
                    _ => {
                        return Err(Refusal::new(
                            "a `Set.single` of something other than a whole number or a string",
                        ));
                    }
                };
                let ty = Type::Set(item);
                let empty = self.builder.emit(ty, Op::DictNew);
                let member = self.set_member();
                Ok(self
                    .builder
                    .emit(ty, Op::DictInsert(empty, element, member)))
            }
            "Set.to_list" | "Set.len" | "Set.is_empty" => {
                let [set] = arguments.exactly(1)?[..] else {
                    return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
                };
                let set = self.value(set)?;
                let item = self.set_item(set, name)?;
                Ok(match name {
                    "Set.to_list" => self
                        .builder
                        .emit(Type::Vector { item, array: true }, Op::DictKeys(set)),
                    "Set.len" => self.builder.emit(Type::I64, Op::DictLen(set)),
                    _ => {
                        let count = self.builder.emit(Type::I64, Op::DictLen(set));
                        let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
                        self.test(Compare::Eq, Item::I64, count, zero)
                    }
                })
            }
            "Set.contains" | "Set.insert" | "Set.remove" => {
                let [set, element] = arguments.exactly(2)?[..] else {
                    return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
                };
                let set = self.value(set)?;
                let element = self.value(element)?;
                let item = self.set_item(set, name)?;
                let element = self.convert_scalar(element, item)?;
                let ty = self.builder.ty(set);
                Ok(match name {
                    "Set.contains" => self
                        .builder
                        .emit(Type::Bool, Op::DictContains(set, element)),
                    "Set.insert" => {
                        let member = self.set_member();
                        self.builder.emit(ty, Op::DictInsert(set, element, member))
                    }
                    _ => self.builder.emit(ty, Op::DictRemove(set, element)),
                })
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
            "Num.count_one_bits"
            | "Num.count_leading_zero_bits"
            | "Num.count_trailing_zero_bits" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed bit count"));
                };
                let a = self.value(a)?;
                // On R numbers the library counts 32 bits and gives an R integer.
                if self.builder.ty(a) != Type::I64 {
                    return Err(Refusal::new("a bit count of an R number"));
                }
                let count = match name {
                    "Num.count_one_bits" => BitCount::Ones,
                    "Num.count_leading_zero_bits" => BitCount::LeadingZeros,
                    _ => BitCount::TrailingZeros,
                };
                Ok(self.builder.emit(Type::I64, Op::CountBits(count, a)))
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
            "Num.exp" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `Num.exp`"));
                };
                let a = self.value(a)?;
                let a = match self.builder.ty(a) {
                    Type::Int | Type::Dbl => self.as_double(a)?,
                    _ => return Err(Refusal::new("a `Num.exp` of something other than an F64")),
                };
                let power = self
                    .builder
                    .emit(Type::Dbl, Op::Math(Math::Exp, Item::Dbl, a));
                self.fail_if_nan(power);
                Ok(power)
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
            "Num.truncate" => {
                let [a] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `Num.truncate`"));
                };
                let a = self.value(a)?;
                if self.builder.ty(a) != Type::Dbl {
                    return Err(Refusal::new(
                        "a `Num.truncate` of something other than an F64",
                    ));
                }
                let whole = self
                    .builder
                    .emit(Type::Dbl, Op::Math(Math::Trunc, Item::Dbl, a));
                Ok(self.builder.emit(Type::I64, Op::ToI64(whole)))
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
            "Num.is_zero" | "Num.is_positive" | "Num.is_negative" | "Num.is_even"
            | "Num.is_odd" | "Num.is_finite" | "Num.is_infinite" | "Num.neg" | "Num.floor"
            | "Num.ceiling" | "Num.to_i64" | "Num.log" | "Num.sin" | "Num.cos" | "Num.tan"
            | "Num.atan" | "Num.asin" | "Num.acos" => self.number_function(name, &arguments),
            "Num.is_lt" | "Num.is_gt" | "Num.is_lte" | "Num.is_gte" | "Num.add" | "Num.sub"
            | "Num.mul" | "Num.div" | "Num.div_trunc" | "Num.rem" | "Num.abs_diff"
            | "Num.is_multiple_of" => self.number_operation(name, &arguments),
            "Num.add_wrap" | "Num.sub_wrap" | "Num.mul_wrap" | "Num.add_saturated"
            | "Num.sub_saturated" | "Num.mul_saturated" | "Num.add_checked" | "Num.sub_checked"
            | "Num.mul_checked" | "Num.div_ceil" => self.overflowing(name, &arguments),
            "Bool.and" | "Bool.or" | "Bool.not" | "Bool.is_eq" | "Bool.is_not_eq" => {
                self.boolean(name, &arguments)
            }
            "List.first" | "List.last" | "List.get" => self.list_get(name, &arguments),
            "Result.is_ok" | "Result.is_err" => self.result_is(name == "Result.is_ok", &arguments),
            "Str.is_empty" => {
                let [string] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `Str.is_empty`"));
                };
                let string = self.value(string)?;
                if self.builder.ty(string) != Type::Str {
                    return Err(Refusal::new(
                        "a `Str.is_empty` of something other than a Str",
                    ));
                }
                let bytes = self.builder.emit(Type::I64, Op::ByteCount(string));
                let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
                Ok(self.test(Compare::Eq, Item::I64, bytes, zero))
            }
            "List.count_if"
            | "List.walk"
            | "List.walk_backwards"
            | "List.walk_until"
            | "List.walk_backwards_until"
            | "List.walk_with_index"
            | "List.walk_with_index_until"
            | "List.walk_from"
            | "List.walk_from_until"
            | "List.walk_try"
            | "Dict.walk"
            | "Dict.walk_until"
            | "Set.walk"
            | "Set.walk_until"
            | "Str.walk_utf8"
            | "Str.walk_utf8_with_index" => self.walk(name, &arguments),
            "List.any" | "List.all" => self.any_all(name == "List.all", &arguments),
            "List.product" => self.product(&arguments),
            "List.single" | "List.prepend" => self.single_or_prepend(name, &arguments),
            "List.set" => self.list_set(&arguments),
            "List.drop_at" => self.drop_at(&arguments),
            "List.replace" => self.replace(&arguments),
            "List.update" => self.list_update(&arguments),
            "List.split_at" => self.split_at(&arguments),
            "List.split_first" | "List.split_last" => {
                self.split_around(name == "List.split_last", &arguments)
            }
            "List.starts_with" | "List.ends_with" => {
                self.affix(name == "List.starts_with", &arguments)
            }
            "List.intersperse" => self.intersperse(&arguments),
            "List.join" => self.join(&arguments),
            "List.for_each" => self.for_each(&arguments),
            "Num.div_checked"
            | "Num.div_trunc_checked"
            | "Num.div_ceil_checked"
            | "Num.rem_checked"
            | "Num.sqrt_checked"
            | "Num.log_checked" => self.checked_number(name, &arguments),
            "Num.is_approx_eq" => self.is_approx_eq(&arguments),
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
                // `List.repeat` of a list builds a list of lists, which base R's `rep` cannot.
                let ty = self.builder.ty(value);
                let item = if name == "List.repeat" {
                    ty.held_item()
                } else {
                    ty.item()
                }
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
                            .emit(Type::Dbl, Op::Reduce(Reduce::ListSum, Item::Dbl, a));
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
                let fixed_decimals = |format: &str| {
                    format
                        .strip_prefix("%.")
                        .and_then(|rest| rest.strip_suffix('f'))
                        .and_then(|digits| digits.parse::<i32>().ok())
                        .filter(|&digits| (0..=15).contains(&digits))
                };
                let known = match arguments.exactly(2).as_deref() {
                    Ok(&[format, value]) => constant_string(format)
                        .filter(|format| {
                            matches!(format.as_str(), "%d" | "%s")
                                || fixed_decimals(format).is_some()
                        })
                        .map(|format| (format, value)),
                    _ => None,
                };
                // R formats every other form, where the call is one R may make.
                let Some((format, value)) = known else {
                    return match r_use {
                        Some(r_use) => self.r_call(call, function, r_use),
                        None => Err(Refusal::new(
                            "a `sprintf` format the typed core does not know",
                        )),
                    };
                };
                let value = self.value(value)?;
                let ty = self.builder.ty(value);
                match (format.as_str(), fixed_decimals(&format), ty) {
                    ("%d", _, Type::Int) | ("%s", _, Type::Int | Type::Dbl | Type::Str) => {
                        self.as_string(value)
                    }
                    (_, Some(decimals), Type::Dbl | Type::Int) => {
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
                    Type::Str => {
                        let test = if starts {
                            StrTest::StartsWith
                        } else {
                            StrTest::EndsWith
                        };
                        Ok(self.builder.emit(Type::Lgl, Op::StrTest(test, a, affix)))
                    }
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
                let code = vector;
                let vector = self.value(vector)?;
                let ty = self.builder.ty(vector);
                if ty == Type::Table {
                    if name == "[" {
                        return Err(Refusal::new(
                            "a `[` of a table, which typed code reads with the table API's verbs",
                        ));
                    }
                    let name = self.value(index)?;
                    return self.column(call, r_use, (code, vector), (index, name));
                }
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
                if name == "["
                    && let Type::Vector { array: true, .. } = ty
                {
                    return match self.builder.ty(index) {
                        Type::I64
                        | Type::Vector {
                            item: Item::I64 | Item::Int,
                            ..
                        } => Ok(self.builder.emit(ty, Op::Pick(vector, index))),
                        // `to_i64` fails on an `NA` integer, which the interpreter rejects.
                        Type::Int | Type::Dbl => {
                            let index = self.to_i64(index)?;
                            Ok(self.builder.emit(ty, Op::Pick(vector, index)))
                        }
                        _ => Err(Refusal::new(
                            "a list indexed by something other than positions",
                        )),
                    };
                }
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
                let with_capacity = self.is_library_call(list, "Dict.with_capacity", 1)?
                    || self.is_library_call(list, "Set.with_capacity", 1)?;
                if with_capacity {
                    // Native collections keep no reserved room a program could read, so the
                    // room only has to be one the interpreter accepts: below 2^30.
                    let [capacity] = Arguments::of(list).exactly(1)?[..] else {
                        return Err(Refusal::new("a malformed `with_capacity`"));
                    };
                    let capacity = self.value(capacity)?;
                    let capacity = self.to_i64(capacity)?;
                    let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
                    let limit = self
                        .builder
                        .emit(Type::I64, Op::Const(Constant::I64(1 << 30)));
                    let negative = self.builder.emit(
                        Type::Lgl,
                        Op::Compare(Compare::Lt, Item::I64, capacity, zero),
                    );
                    self.fail_if(negative);
                    let huge = self.builder.emit(
                        Type::Lgl,
                        Op::Compare(Compare::Ge, Item::I64, capacity, limit),
                    );
                    self.fail_if(huge);
                }
                if with_capacity
                    || self.is_library_call(list, "Dict.empty", 0)?
                    || self.is_library_call(list, "Set.empty", 0)?
                {
                    let ty = constant_string(hint)
                        .as_deref()
                        .and_then(|annotation| keyed_type(annotation))
                        .ok_or(Refusal::new(
                            "an empty dictionary or set of an unknown type",
                        ))?;
                    return Ok(self.builder.emit(ty, Op::DictNew));
                }
                if self.is_library_call(list, "Num.from_r", 1)? {
                    self.checked_items = match constant_string(hint).as_deref() {
                        Some("I64") => Some(Item::I64),
                        Some("F64") => Some(Item::Dbl),
                        _ => None,
                    };
                    let value = self.value(list);
                    self.checked_items = None;
                    return value;
                }
                if self.is_library_call(list, "List.from_r", 1)?
                    || self.is_library_call(list, "List.from_r_or", 2)?
                    || self.is_library_call(list, "List.from_r_missing", 1)?
                {
                    self.checked_items = match constant_string(hint).as_deref() {
                        Some("I64") => Some(Item::I64),
                        Some("F64") => Some(Item::Dbl),
                        Some("Str") => Some(Item::Str),
                        _ => None,
                    };
                    let value = self.value(list);
                    self.checked_items = None;
                    return value;
                }
                let mut settled = false;
                for (name, arity) in SETTLED_EMPTY {
                    settled = settled || self.is_library_call(list, name, arity)?;
                }
                self.settled_empty = settled;
                let value = self.value(list);
                self.settled_empty = false;
                value
            }
            ".ry_array" => self.list_literal(&arguments, None, None),
            "List.append" => self.append(&arguments),
            "List.append_if_ok" | "List.prepend_if_ok" => self.add_if_ok(name, &arguments),
            "List.keep_if" | "List.drop_if" | "List.keep_if_try" => self.keep_if(name, &arguments),
            "List.find_first"
            | "List.find_first_index"
            | "List.find_last"
            | "List.find_last_index" => self.find_first(name, &arguments),
            "List.contains" => self.contains(&arguments),
            "List.reverse" => self.reverse(&arguments),
            "List.max" | "List.min" => self.list_extreme(name, &arguments),
            "List.join_map" => self.join_map(&arguments),
            "List.chunks_of" => self.chunks_of(&arguments),
            "List.map2" | "List.map3" | "List.map4" | "List.map_try" => {
                self.map_many(name, &arguments)
            }
            "Str.split_first" | "Str.split_last" | "Str.replace_first" | "Str.replace_last" => {
                self.around_pattern(name, &arguments)
            }
            "Str.replace_each" => {
                let [string, pattern, replacement] = arguments.exactly(3)?[..] else {
                    return Err(Refusal::new("a malformed `Str.replace_each`"));
                };
                let string = self.value(string)?;
                let pattern = self.value(pattern)?;
                let replacement = self.value(replacement)?;
                if [string, pattern, replacement]
                    .iter()
                    .any(|&value| self.builder.ty(value) != Type::Str)
                {
                    return Err(Refusal::new(
                        "a `Str.replace_each` of something other than strings",
                    ));
                }
                Ok(self
                    .builder
                    .emit(Type::Str, Op::ReplaceEach(string, pattern, replacement)))
            }
            "List.concat" | "List.concat_utf8" => self.concat(name, &arguments),
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
            "lapply" | "vapply" | "List.map" | "List.map_with_index" => self.map(name, &arguments),
            "List.sort_with" => self.sort_with(&arguments),
            ".ry_record" | ".ry_tuple" | ".ry_tag" => self.construct(name, &arguments),
            ".ry_update" => self.update(&arguments),
            "$" => {
                let [object, field] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed `$`"));
                };
                let code = object;
                let object = self.value(object)?;
                let symbol = match field.kind() {
                    SexpType::Symbol => field,
                    SexpType::String => {
                        let text =
                            rvalues::string_at(field, 0).ok_or(Refusal::new("an NA field name"))?;
                        rvalues::install(&text).map_err(|_| Refusal::new("a field name"))?
                    }
                    _ => return Err(Refusal::new("a `$` of a computed field")),
                };
                if self.builder.ty(object) == Type::Table {
                    let name = self.string(&rvalues::symbol_name(symbol));
                    return self.column(call, r_use, (code, object), (field, name));
                }
                let Type::Record(shape) = self.builder.ty(object) else {
                    return Err(Refusal::new("a `$` of something other than a record"));
                };
                let index = interp::heap()
                    .layouts
                    .get(shapes::shape(shape).layout)
                    .and_then(|layout| layout.names().iter().position(|&name| name == symbol))
                    .ok_or(Refusal::new("a field the record does not have"))?;
                self.field(object, shape, index)
            }
            "Result.with_default" => self.with_default(&arguments),
            "Result.map" | "Result.map_err" | "Result.map_both" | "Result.and_then"
            | "Result.on_err" => self.result_map(name, &arguments),
            "List.keep_oks" | "List.keep_errs" => {
                self.keep_results(name == "List.keep_oks", &arguments)
            }
            "Dict.get" => self.dict_get(&arguments),
            "Dict.insert" => self.dict_insert(&arguments),
            "Dict.to_list" => self.dict_to_list(&arguments),
            "Dict.from_list" => self.dict_from_list(&arguments),
            "Dict.map" | "Set.map" => self.dict_map(name, &arguments),
            "Dict.keep_if" | "Dict.drop_if" | "Dict.keep_shared" | "Dict.remove_all"
            | "Set.keep_if" | "Set.drop_if" | "Set.intersection" | "Set.difference" => {
                self.dict_filter(name, &arguments)
            }
            "Dict.insert_all" | "Set.union" => self.dict_insert_all(name, &arguments),
            "Dict.join_map" | "Set.join_map" => self.dict_join_map(name, &arguments),
            "List.reserve" | "Dict.reserve" | "Set.reserve" => {
                let [collection, spare] = arguments.exactly(2)?[..] else {
                    return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
                };
                let collection = self.value(collection)?;
                let collection = self.capacity_operand(name, collection)?;
                let spare = self.value(spare)?;
                let spare = self.to_i64(spare)?;
                // The interpreter answers an empty list with a fresh logical array and raises
                // an error for a negative count or room past 2^30, so those cases run there.
                let count = if name == "List.reserve" {
                    self.builder.emit(Type::I64, Op::Length(collection))
                } else {
                    self.builder.emit(Type::I64, Op::DictLen(collection))
                };
                let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
                let limit = self
                    .builder
                    .emit(Type::I64, Op::Const(Constant::I64(1 << 30)));
                if name == "List.reserve" {
                    let empty = self
                        .builder
                        .emit(Type::Lgl, Op::Compare(Compare::Eq, Item::I64, count, zero));
                    self.fail_if(empty);
                }
                let negative = self
                    .builder
                    .emit(Type::Lgl, Op::Compare(Compare::Lt, Item::I64, spare, zero));
                self.fail_if(negative);
                let huge = self
                    .builder
                    .emit(Type::Lgl, Op::Compare(Compare::Ge, Item::I64, spare, limit));
                self.fail_if(huge);
                let room = self
                    .builder
                    .emit(Type::I64, Op::Arith(Arith::Add, Item::I64, count, spare));
                let crowded = self
                    .builder
                    .emit(Type::Lgl, Op::Compare(Compare::Ge, Item::I64, room, limit));
                self.fail_if(crowded);
                Ok(collection)
            }
            "List.release_excess_capacity"
            | "Dict.release_excess_capacity"
            | "Set.release_excess_capacity" => {
                let [collection] = arguments.exactly(1)?[..] else {
                    return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
                };
                let collection = self.value(collection)?;
                self.capacity_operand(name, collection)
            }
            "Dict.clear" => {
                let [dict] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `Dict.clear`"));
                };
                let dict = self.value(dict)?;
                let ty = self.builder.ty(dict);
                if !matches!(ty, Type::Dict { .. }) {
                    return Err(Refusal::new(
                        "a `Dict.clear` of something other than a dictionary",
                    ));
                }
                Ok(self.builder.emit(ty, Op::DictNew))
            }
            "Num.to_i64_checked" => self.to_i64_checked(&arguments),
            "List.split_on" | "List.split_on_list" => self.split_on(name, &arguments),
            "Result.map2" => self.result_map2(&arguments),
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
            "Dict.is_empty" => {
                let [dict] = arguments.exactly(1)?[..] else {
                    return Err(Refusal::new("a malformed `Dict.is_empty`"));
                };
                let dict = self.value(dict)?;
                if !matches!(self.builder.ty(dict), Type::Dict { .. }) {
                    return Err(Refusal::new(
                        "a `Dict.is_empty` of something other than a dictionary",
                    ));
                }
                let count = self.builder.emit(Type::I64, Op::DictLen(dict));
                let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
                Ok(self.test(Compare::Eq, Item::I64, count, zero))
            }
            "Dict.single" => {
                let [key, value] = arguments.exactly(2)?[..] else {
                    return Err(Refusal::new("a malformed `Dict.single`"));
                };
                let key = self.value(key)?;
                let value = self.value(value)?;
                self.usable(value)?;
                let key_item = match self.builder.ty(key) {
                    Type::Str => Item::Str,
                    Type::I64 => Item::I64,
                    Type::Dbl => Item::Dbl,
                    _ => {
                        return Err(Refusal::new(
                            "a `Dict.single` of a key that is not an `I64`, an `F64` or a `Str`",
                        ));
                    }
                };
                let value_item = self.builder.ty(value).held_item().ok_or(Refusal::new(
                    "a `Dict.single` of a value a dictionary cannot hold",
                ))?;
                let ty = Type::Dict {
                    key: key_item,
                    value: value_item,
                };
                let empty = self.builder.emit(ty, Op::DictNew);
                Ok(self.builder.emit(ty, Op::DictInsert(empty, key, value)))
            }
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
            | "Str.to_utf16"
            | "Str.to_utf32"
            | "Str.with_ascii_uppercased"
            | "Str.with_ascii_lowercased"
            | "Str.trim"
            | "Str.trim_start"
            | "Str.trim_end" => {
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
                    "Str.to_utf8" => self.encode(Encoding::Utf8, a),
                    "Str.to_utf16" => self.encode(Encoding::Utf16, a),
                    "Str.to_utf32" => self.encode(Encoding::Utf32, a),
                    "Str.with_ascii_uppercased" => {
                        self.builder.emit(Type::Str, Op::Text(Text::AsciiUpper, a))
                    }
                    "Str.with_ascii_lowercased" => {
                        self.builder.emit(Type::Str, Op::Text(Text::AsciiLower, a))
                    }
                    "Str.trim" => self.builder.emit(Type::Str, Op::Text(Text::Trim, a)),
                    "Str.trim_start" => self.builder.emit(Type::Str, Op::Text(Text::TrimStart, a)),
                    _ => self.builder.emit(Type::Str, Op::Text(Text::TrimEnd, a)),
                })
            }
            "Str.from_utf8"
            | "Str.from_utf8_lossy"
            | "Str.from_utf16"
            | "Str.from_utf16_lossy"
            | "Str.from_utf32"
            | "Str.from_utf32_lossy" => self.decode(name, &arguments),
            "Str.starts_with"
            | "Str.ends_with"
            | "Str.contains"
            | "Str.caseless_ascii_equals"
            | "Str.drop_prefix"
            | "Str.drop_suffix"
            | "Str.with_prefix"
            | "Str.split_on"
            | "Str.join_with" => {
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
                    (
                        "Str.starts_with"
                        | "Str.ends_with"
                        | "Str.contains"
                        | "Str.caseless_ascii_equals",
                        Type::Str,
                    ) => {
                        let test = match name {
                            "Str.starts_with" => StrTest::StartsWith,
                            "Str.ends_with" => StrTest::EndsWith,
                            "Str.contains" => StrTest::Contains,
                            _ => StrTest::CaselessAsciiEquals,
                        };
                        Ok(self.builder.emit(Type::Lgl, Op::StrTest(test, a, b)))
                    }
                    ("Str.drop_prefix" | "Str.drop_suffix", Type::Str) => Ok(self
                        .builder
                        .emit(Type::Str, Op::DropAffix(name == "Str.drop_prefix", a, b))),
                    ("Str.with_prefix", Type::Str) => {
                        let empty = self.string("");
                        Ok(self
                            .builder
                            .emit(Type::Str, Op::Concat(SmallVec::from_slice(&[b, a]), empty)))
                    }
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
            "Num.from_r"
            | "Str.from_r"
            | "Bool.from_r"
            | "List.from_r"
            | "List.from_r_or"
            | "List.from_r_missing" => {
                let checked = self.checked_items.take();
                let (inner, default) = if name == "List.from_r_or" {
                    let mut positional = arguments.positional();
                    let vector = arguments.named("vector").or_else(|| positional.next());
                    let default = arguments.named("default").or_else(|| positional.next());
                    match (vector, default) {
                        (Some(vector), Some(default))
                            if arguments.items.len() == 2
                                && arguments.only_names(&["vector", "default"]) =>
                        {
                            (vector, Some(default))
                        }
                        _ => return Err(Refusal::new("a conversion of an unsupported form")),
                    }
                } else {
                    let [inner] = arguments.exactly(1)?[..] else {
                        return Err(Refusal::new("a conversion of an unsupported form"));
                    };
                    (inner, None)
                };
                // The interpreter computes the default after the vector, and compiled code before
                // it, which agree while the default calls no R function.
                let default_reads = self.param_reads.len();
                let default = match default {
                    Some(code) => {
                        let calls = self.r_calls.len();
                        let blocks = self.builder.function.blocks.len();
                        let value = self.value(code)?;
                        if self.r_calls.len() != calls {
                            return Err(Refusal::new(
                                "it calls R in the default of `List.from_r_or`, which R computes \
                                 after the vector: compute the default into a variable first",
                            ));
                        }
                        // The reads move to where R computes the default, which keeps their
                        // paths only for a default without branches.
                        if self.builder.function.blocks.len() != blocks
                            && self.param_reads.len() != default_reads
                        {
                            return Err(Refusal::new(
                                "it reads a parameter on some paths of the default of \
                                 `List.from_r_or`, which R computes after the vector: compute the \
                                 default into a variable first",
                            ));
                        }
                        Some(value)
                    }
                    None => None,
                };
                self.r_use = Some(RUse::Converted(
                    Conversion {
                        call,
                        function,
                        checked,
                        default: default.map(|default| self.builder.ty(default)),
                    },
                    default,
                ));
                let default_end = self.param_reads.len();
                let value = self.value(inner);
                self.r_use = None;
                let value = value?;
                if self.converted.take() == Some(value) {
                    // The default's reads come after the R call, where R computes it.
                    let block = self.builder.current;
                    let position = self
                        .builder
                        .function
                        .blocks
                        .get(block.0 as usize)
                        .map_or(0, |block| block.insts.len());
                    for read in self
                        .param_reads
                        .iter_mut()
                        .take(default_end)
                        .skip(default_reads)
                    {
                        read.block = block;
                        read.position = position as u32;
                    }
                    return Ok(value);
                }
                // A conversion of a value that already has the type it gives is that value.
                let ty = self.builder.ty(value);
                let identity = match name {
                    "Num.from_r" => matches!(ty, Type::Dbl | Type::I64),
                    "Str.from_r" => ty == Type::Str,
                    "Bool.from_r" => ty == Type::Bool,
                    // An `Array` holds no `NA`, so it has nothing to fill, but its items are not
                    // results.
                    "List.from_r_missing" => false,
                    _ => matches!(ty, Type::Vector { array: true, .. }),
                };
                if identity {
                    return Ok(value);
                }
                // A count such as `nchar(x)` is an R integer the typed core computed itself.
                if name == "Num.from_r" && ty == Type::Int {
                    return self.to_i64(value);
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
                ("lapply" | "List.map" | "List.map_with_index", &[vector, function]) => {
                    (vector, function, None)
                }
                _ => return Err(Refusal::new("a map of an unsupported form")),
            };
        let settled = std::mem::take(&mut self.settled_empty);
        let with_index = name == "List.map_with_index";
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
        // cannot type from no items at all unless a `.ry_items` gives it the storage.
        let to_array = name.starts_with("List.") && matches!(ty, Type::Vector { array: true, .. });
        if with_index && !to_array {
            return Err(Refusal::new(
                "a `List.map_with_index` of something other than a ry list",
            ));
        }
        let held = self.builder.emit(ty, Op::Copy(vector));
        let count = self.builder.emit(Type::Int, Op::Length(held));
        if to_array && !settled {
            let zero = self.int(0);
            let empty = self.builder.emit(Type::Bool, Op::Same(count, zero));
            self.fail_if(empty);
        }
        let preheader = self.builder.current;
        // The result's type is known only once the body is lowered, so the output vector is
        // allocated in the preheader afterwards.
        let output = self.builder.local(Type::Null);
        let positions = self.positions(held);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, positions.counter));
        let mut inputs: SmallVec<[Local; 2]> = SmallVec::from_slice(&[element]);
        if with_index {
            inputs.push(positions.counter);
        }
        let value = self
            .apply(callable, &inputs)?
            .ok_or(Refusal::new("a mapped function that never completes"))?;
        // A ry list holds no `NA`, so a logical a mapped function returns into one must be
        // settled, and an `NA` stops the native code as a missing item does in the interpreter.
        let value = if to_array && self.builder.ty(value) == Type::Lgl {
            self.builder.emit(Type::Bool, Op::Truth(value))
        } else {
            value
        };
        let value_ty = self.builder.ty(value);
        let value_item = if to_array {
            value_ty.held_item()
        } else {
            value_ty.item()
        }
        .ok_or(Refusal::new(
            "a mapped function whose value is not a scalar",
        ))?;
        let result_item = declared.unwrap_or(value_item);
        let settled = value_ty == Type::Bool && result_item == Item::Lgl;
        let value = if settled {
            value
        } else {
            self.convert_scalar(value, result_item)?
        };
        let output_ty = if name == "vapply" {
            Type::vector(result_item)
        } else if to_array {
            if result_item == Item::Int || (result_item == Item::Lgl && !settled) {
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
            .emit(output_ty, Op::Set(output, positions.counter, value));
        self.builder.push(Some(output), Op::Copy(updated));
        self.next_position(positions);
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
        self.builder.switch_to(positions.exit);
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
        let greater = self.shape_id(greater);
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
    /// `List.walk_from` starts at a position, `Dict.walk` steps with each key and value, and
    /// `List.walk_try` leaves with the first `:Err` a step gives and wraps the state in `:Ok`.
    unsafe fn walk(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let counting = name == "List.count_if";
        let backwards = name.starts_with("List.walk_backwards");
        let with_index = name.starts_with("List.walk_with_index")
            || name == "Str.walk_utf8_with_index";
        let bytes = name.starts_with("Str.walk_utf8");
        let from = name.starts_with("List.walk_from");
        let over_dict = name.starts_with("Dict.") || name.starts_with("Set.");
        let until = name.ends_with("_until");
        let trying = name == "List.walk_try";
        let (list, start, state, function) = match (
            counting,
            from,
            &arguments.exactly(arguments.items.len())?[..],
        ) {
            (true, _, &[list, predicate]) => (list, None, None, predicate),
            (false, false, &[list, state, f]) => (list, None, Some(state), f),
            (false, true, &[list, index, state, f]) => (list, Some(index), Some(state), f),
            _ => return Err(Refusal::new("a walk of an unsupported form")),
        };
        // A walk over `List.range(from, to)` counts from `from` to `to` and builds no list.
        let range = if !backwards
            && !with_index
            && !from
            && !over_dict
            && self.is_library_call(list, "List.range", 2)?
        {
            self.counted_range(list)?
        } else {
            None
        };
        let (list, ty) = match range {
            Some(_) => (None, Type::Null),
            None => {
                let mut list = self.value(list)?;
                if bytes {
                    if self.builder.ty(list) != Type::Str {
                        return Err(Refusal::new(
                            "a `Str.walk_utf8` of something other than a string",
                        ));
                    }
                    list = self.encode(Encoding::Utf8, list);
                }
                let ty = self.builder.ty(list);
                let walkable = if over_dict {
                    ty.entry_items().is_some()
                } else {
                    matches!(ty, Type::Vector { array: true, .. } | Type::List(_))
                };
                if !walkable {
                    return Err(Refusal::new("a walk over something that is not a list"));
                }
                (Some(list), ty)
            }
        };
        // A start before position 1 is an error, which the interpreter raises.
        let start = match start {
            Some(index) => {
                let index = self.value(index)?;
                let index = self.to_i64(index)?;
                let one = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
                let before = self
                    .builder
                    .emit(Type::Lgl, Op::Compare(Compare::Lt, Item::I64, index, one));
                self.fail_if(before);
                Some(index)
            }
            None => None,
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
            && let Some(list) = list
        {
            return Ok(self.count_true(list));
        }
        // A predicate that only compares the item with numbers means the same for the whole
        // list as for each item. It is lowered once with its parameter standing for the list,
        // and the fusion pass then counts from the comparisons' inputs in one pass, where the
        // loop below would test one item at a time.
        if counting
            && ty
                == (Type::Vector {
                    item: Item::Dbl,
                    array: true,
                })
            && let Callable::Inline(formals, body, scope) = callable
            && scope + 1 == self.scopes.len()
            && let [(name, _)] = rvalues::pairlist_items(formals)[..]
            && interp::annotations::declared_i64(formals, body).0.first() != Some(&true)
            && self.compares_item(body, name)
            // The predicate's annotations must admit an `F64` item and a `Bool` answer, as they
            // must when it is inlined per item; otherwise the loop below refuses the call.
            && interp::annotations::admits_inline(
                formals,
                body,
                &[inline_storage(Type::Dbl)],
                inline_storage(Type::Lgl),
            )
            && let Some(list) = list
        {
            let mut scope = self.new_scope();
            scope.names.insert(name, Binding::Value(list));
            self.scopes.push(scope);
            let mask = self.expression(body);
            self.scopes.pop();
            let mask = mask?.ok_or(Refusal::new("a predicate that never completes"))?;
            return Ok(self.count_true(mask));
        }
        let state = match state {
            Some(state)
                if self.is_library_call(state, "Dict.empty", 0)?
                    || self.is_library_call(state, "Set.empty", 0)? =>
            {
                // An empty dictionary's or set's type is the one the step function declares for
                // its state.
                let (Callable::Inline(formals, body, _) | Callable::Captured(formals, body, _)) =
                    callable
                else {
                    return Err(Refusal::new(
                        "a walk from an empty collection with a named step",
                    ));
                };
                let ty = interp::annotations::formal_annotation(formals, body, 0)
                    .as_deref()
                    .and_then(|annotation| keyed_type(annotation))
                    .ok_or(Refusal::new(
                        "a walk from an empty collection whose state type is unknown",
                    ))?;
                self.builder.emit(ty, Op::DictNew)
            }
            Some(state) => self.value(state)?,
            None => self.builder.emit(Type::I64, Op::Const(Constant::I64(0))),
        };
        // A step that returns more tags than the state holds widens the state, as a variable
        // widens, keyed by the step function.
        let state_ty = self.widened_type(function, self.builder.ty(state));
        let total = self.builder.emit(state_ty, Op::Copy(state));
        let mut entry_value = None;
        let mut failure = None;
        let (positions, element, position) = match (range, list) {
            (Some((start, end, _)), _) => {
                let positions = self.count_up(start, end);
                let element = self.builder.emit(Type::I64, Op::Copy(positions.counter));
                (positions, element, positions.counter)
            }
            (None, Some(dict)) if over_dict => {
                let (keys, values, key, value) = self.entries(dict, name)?;
                let positions = self.positions(keys);
                let element = self
                    .builder
                    .emit(Type::of_item(key), Op::ItemAt(keys, positions.counter));
                // A set's step takes only the element.
                entry_value = values.map(|values| {
                    self.builder
                        .emit(Type::of_item(value), Op::ItemAt(values, positions.counter))
                });
                (positions, element, positions.counter)
            }
            (None, Some(list)) => {
                let item = ty
                    .items()
                    .ok_or(Refusal::new("a walk over something that is not a list"))?;
                let held = self.builder.emit(ty, Op::Copy(list));
                let past_end = backwards.then(|| self.past_end(held));
                let positions = match start {
                    Some(start) => {
                        let count = self.builder.emit(Type::I64, Op::Length(held));
                        self.count_up(start, count)
                    }
                    None => self.positions(held),
                };
                let position = self.position(past_end, positions.counter);
                let element = self
                    .builder
                    .emit(Type::of_item(item), Op::ItemAt(held, position));
                (positions, element, position)
            }
            (None, None) => return Err(Refusal::new("a walk over nothing")),
        };
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
            let mut inputs: SmallVec<[Local; 3]> = SmallVec::from_slice(&[total, element]);
            inputs.extend(entry_value);
            if with_index {
                inputs.push(position);
            }
            let step = self
                .apply(callable, &inputs)?
                .ok_or(Refusal::new("a walk step that never completes"))?;
            if until {
                self.walk_step(step, total, (function, state_ty), positions.exit)?;
            } else if trying {
                let (next, failed) = self.ok_or_leave(step)?;
                if !self.fits_widening(function, state_ty, self.builder.ty(next)) {
                    return Err(Refusal::new("a walk whose state changes type"));
                }
                self.builder.push(Some(total), Op::Copy(next));
                failure = failed;
            } else {
                if !self.fits_widening(function, state_ty, self.builder.ty(step)) {
                    return Err(Refusal::new("a walk whose state changes type"));
                }
                self.builder.push(Some(total), Op::Copy(step));
            }
        }
        self.next_position(positions);
        if trying {
            return self.ok_or_failure(total, failure);
        }
        Ok(total)
    }

    /// A `walk_until` step: `:Continue(state)` sets the walk's `total` and goes on, and
    /// `:Break(state)` sets it and leaves the loop for `exit`.
    unsafe fn walk_step(
        &mut self,
        step: Local,
        total: Local,
        (function, state_ty): (Sexp, Type),
        exit: BlockId,
    ) -> Result<(), Refusal> {
        let (go_on, stop) = (tag_layout("Continue", 1)?, tag_layout("Break", 1)?);
        let (candidates, exhaustive) =
            self.shapes_of(self.builder.ty(step), &[go_on, stop], &[])?;
        // A step that never continues leaves the loop at the first item, and the block after
        // the switch is then unreachable.
        self.switch_shapes(
            step,
            &candidates,
            exhaustive,
            Expected::Free,
            &mut |lowering, shape| {
                let Some(shape) = shape.filter(|&shape| {
                    let layout = shapes::shape(shape).layout;
                    layout == go_on || layout == stop
                }) else {
                    lowering.builder.terminate(Terminator::Fail);
                    let unreachable = lowering.builder.block();
                    lowering.builder.switch_to(unreachable);
                    return Ok(None);
                };
                let next = lowering.field(step, shape, 0)?;
                if !lowering.fits_widening(function, state_ty, lowering.builder.ty(next)) {
                    return Err(Refusal::new("a walk whose state changes type"));
                }
                lowering.builder.push(Some(total), Op::Copy(next));
                if shapes::shape(shape).layout == stop {
                    lowering.builder.terminate(Terminator::Jump(exit));
                    let unreachable = lowering.builder.block();
                    lowering.builder.switch_to(unreachable);
                    return Ok(None);
                }
                Ok(Some(next))
            },
        )?;
        Ok(())
    }

    /// The payload of `result`, a `Result` a callback gave inside a loop, for the code that goes
    /// on with it. An `:Err` is kept in a local and leaves for a block that reads it, both
    /// returned for [`Self::ok_or_failure`], or `None` when the callback never fails.
    unsafe fn ok_or_leave(
        &mut self,
        result: Local,
    ) -> Result<(Local, Option<(Local, BlockId)>), Refusal> {
        let (ok, err) = (tag_layout("Ok", 1)?, tag_layout("Err", 1)?);
        let (candidates, exhaustive) = self.shapes_of(self.builder.ty(result), &[ok, err], &[])?;
        let failures: SmallVec<[u32; 4]> = candidates
            .iter()
            .copied()
            .filter(|&shape| shapes::shape(shape).layout == err)
            .collect();
        let failure = (!failures.is_empty()).then(|| {
            let ty = Type::Tag(shapes::union_of(&failures));
            (self.builder.local(ty), self.builder.block())
        });
        let payload = self.switch_shapes(
            result,
            &candidates,
            exhaustive,
            Expected::Free,
            &mut |lowering, shape| match (shape, failure) {
                (Some(shape), _) if shapes::shape(shape).layout == ok => {
                    lowering.field(result, shape, 0).map(Some)
                }
                (Some(shape), Some((kept, failed))) if shapes::shape(shape).layout == err => {
                    let ty = Type::Tag(shapes::union_of(&[shape]));
                    let narrowed = lowering.builder.emit(ty, Op::Relabel(result, ty));
                    lowering.builder.push(Some(kept), Op::Copy(narrowed));
                    lowering.builder.terminate(Terminator::Jump(failed));
                    let unreachable = lowering.builder.block();
                    lowering.builder.switch_to(unreachable);
                    Ok(None)
                }
                // Something other than a result, which the interpreter rejects.
                _ => Ok(lowering.unreachable()),
            },
        )?;
        let payload = payload.ok_or(Refusal::new("a callback that never gives `:Ok`"))?;
        self.usable(payload)?;
        Ok((payload, failure))
    }

    /// `:Ok(value)` built in the current block, joined with the `:Err` an
    /// [`Self::ok_or_leave`] kept when the callback can fail.
    unsafe fn ok_or_failure(
        &mut self,
        value: Local,
        failure: Option<(Local, BlockId)>,
    ) -> Result<Local, Refusal> {
        let ok = self.tag("Ok", &[value])?;
        let Some((err, failed)) = failure else {
            return Ok(ok);
        };
        let succeeded = self.builder.current;
        self.builder.switch_to(failed);
        self.joined_result(succeeded, ok, err)
    }

    /// `Str.split_first`, `split_last`, `replace_first` and `replace_last`: a search for the
    /// first or the last place a pattern occurs, and the record of the text before and after
    /// it, or the string with that place replaced.
    unsafe fn around_pattern(
        &mut self,
        name: &str,
        arguments: &Arguments,
    ) -> Result<Local, Refusal> {
        let replacing = name.starts_with("Str.replace");
        let mut strings: SmallVec<[Local; 3]> = SmallVec::new();
        for &argument in &arguments.exactly(if replacing { 3 } else { 2 })? {
            let value = self.value(argument)?;
            if self.builder.ty(value) != Type::Str {
                return Err(Refusal(Cow::Owned(format!(
                    "a `{name}` of something other than strings"
                ))));
            }
            strings.push(value);
        }
        let (string, pattern) = (strings[0], strings[1]);
        let at = self.builder.emit(
            Type::I64,
            Op::Find(name.ends_with("_last"), string, pattern),
        );
        let width = self.builder.emit(Type::I64, Op::ByteCount(pattern));
        let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
        let found = self
            .builder
            .emit(Type::Lgl, Op::Compare(Compare::Ge, Item::I64, at, zero));
        // Every string holds an empty pattern, which the library splits at and does not replace.
        let found = if replacing {
            let wide = self
                .builder
                .emit(Type::Lgl, Op::Compare(Compare::Gt, Item::I64, width, zero));
            self.builder.emit(Type::Lgl, Op::And(found, wide))
        } else {
            found
        };
        let found = self.builder.emit(Type::Bool, Op::Truth(found));
        let (hit, missing) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: found,
            then: hit,
            otherwise: missing,
        });
        self.builder.switch_to(hit);
        let before = self
            .builder
            .emit(Type::Str, Op::Substring(string, zero, at));
        let resume = self
            .builder
            .emit(Type::I64, Op::Arith(Arith::Add, Item::I64, at, width));
        let total = self.builder.emit(Type::I64, Op::ByteCount(string));
        let after = self
            .builder
            .emit(Type::Str, Op::Substring(string, resume, total));
        if !replacing {
            let parts = self.record_of(&[("before", before), ("after", after)])?;
            let ok = self.tag("Ok", &[parts])?;
            self.builder.switch_to(missing);
            let reason = self.tag("NotFound", &[])?;
            let err = self.tag("Err", &[reason])?;
            return self.joined_result(hit, ok, err);
        }
        let empty = self.string("");
        let replaced = self.builder.emit(
            Type::Str,
            Op::Concat(SmallVec::from_slice(&[before, strings[2], after]), empty),
        );
        let result = self.builder.local(Type::Str);
        let join = self.builder.block();
        self.builder.push(Some(result), Op::Copy(replaced));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(missing);
        self.builder.push(Some(result), Op::Copy(string));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(join);
        Ok(result)
    }

    /// One past the last position of `list`, from which a loop counting up from 1 counts the
    /// positions down.
    fn past_end(&mut self, list: Local) -> Local {
        let count = self.builder.emit(Type::I64, Op::Length(list));
        let one = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
        self.builder
            .emit(Type::I64, Op::Arith(Arith::Add, Item::I64, count, one))
    }

    /// The position a loop's `counter` stands for: itself, or counted from the end when the
    /// loop runs backwards from `past_end`.
    fn position(&mut self, past_end: Option<Local>, counter: Local) -> Local {
        match past_end {
            Some(past_end) => self.builder.emit(
                Type::I64,
                Op::Arith(Arith::Sub, Item::I64, past_end, counter),
            ),
            None => counter,
        }
    }

    /// `List.any(list, predicate)`, or `List.all`, in a loop that leaves at the first item that
    /// decides the answer.
    unsafe fn any_all(&mut self, all: bool, arguments: &Arguments) -> Result<Local, Refusal> {
        let name = if all { "List.all" } else { "List.any" };
        let [list, predicate] = arguments.exactly(2)?[..] else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        let (list, ty) = self.known_list(list, name)?;
        let item = ty.items().ok_or(Refusal(Cow::Owned(format!(
            "a `{name}` of something other than a ry list"
        ))))?;
        let callable = self.function_argument(predicate)?;
        let result = self.builder.local(Type::Lgl);
        let start = self
            .builder
            .emit(Type::Lgl, Op::Const(Constant::Lgl(i32::from(all))));
        self.builder.push(Some(result), Op::Copy(start));
        let held = self.builder.emit(ty, Op::Copy(list));
        let positions = self.positions(held);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, positions.counter));
        let answer = self.predicate(callable, element)?;
        let (decided, next) = (self.builder.block(), self.builder.block());
        let (then, otherwise) = if all {
            (next, decided)
        } else {
            (decided, next)
        };
        self.builder.terminate(Terminator::Branch {
            condition: answer,
            then,
            otherwise,
        });
        self.builder.switch_to(decided);
        let flipped = self
            .builder
            .emit(Type::Lgl, Op::Const(Constant::Lgl(i32::from(!all))));
        self.builder.push(Some(result), Op::Copy(flipped));
        self.builder.terminate(Terminator::Jump(positions.exit));
        self.builder.switch_to(next);
        self.next_position(positions);
        Ok(self.builder.emit(Type::Bool, Op::Truth(result)))
    }

    /// `List.product(list)` of `I64`s or `F64`s, multiplied in order from 1. An `I64` overflow
    /// and a `NaN` product give up, and the interpreter raises the error.
    unsafe fn product(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list] = arguments.exactly(1)?[..] else {
            return Err(Refusal::new("a malformed `List.product`"));
        };
        let (list, ty) = self.known_list(list, "List.product")?;
        let (item, one) = match ty.items() {
            Some(Item::I64) => (Item::I64, Constant::I64(1)),
            Some(Item::Dbl) => (Item::Dbl, Constant::Dbl(1.0)),
            _ => {
                return Err(Refusal::new(
                    "a `List.product` of something other than numbers",
                ));
            }
        };
        let item_ty = Type::of_item(item);
        let start = self.builder.emit(item_ty, Op::Const(one));
        let total = self.builder.emit(item_ty, Op::Copy(start));
        let held = self.builder.emit(ty, Op::Copy(list));
        let positions = self.positions(held);
        let element = self
            .builder
            .emit(item_ty, Op::ItemAt(held, positions.counter));
        let next = self
            .builder
            .emit(item_ty, Op::Arith(Arith::Mul, item, total, element));
        self.builder.push(Some(total), Op::Copy(next));
        self.next_position(positions);
        if item == Item::Dbl {
            self.fail_if_nan(total);
        }
        Ok(total)
    }

    /// `List.single(value)`, or `List.prepend(list, value)` as that list extended by the rest.
    unsafe fn single_or_prepend(
        &mut self,
        name: &str,
        arguments: &Arguments,
    ) -> Result<Local, Refusal> {
        let (list, value) = match arguments.exactly(if name == "List.single" { 1 } else { 2 })?[..]
        {
            [value] => (None, value),
            [list, value] => (Some(list), value),
            _ => return Err(Refusal(Cow::Owned(format!("a malformed `{name}`")))),
        };
        let list = list.map(|list| self.known_list(list, name)).transpose()?;
        let value = self.value(value)?;
        self.prepend_value(list, value)
    }

    /// A fresh list of `value` followed by the items of `list`, if any.
    unsafe fn prepend_value(
        &mut self,
        list: Option<(Local, Type)>,
        value: Local,
    ) -> Result<Local, Refusal> {
        self.usable(value)?;
        let item = self
            .builder
            .ty(value)
            .held_item()
            .ok_or(Refusal::new("a list of something a list cannot hold"))?;
        let single_ty = Type::Vector { item, array: true };
        // A tag of another shape widens the item union, as an append does.
        let list = match list {
            Some((list, ty)) => Some((
                list,
                joined(ty, single_ty)
                    .ok_or(Refusal::new("a `List.prepend` of an item of another type"))?,
            )),
            None => None,
        };
        let empty = self.empty_list(item);
        let single = self.builder.emit(single_ty, Op::Push(empty, value));
        Ok(match list {
            Some((list, ty)) => self.builder.emit(ty, Op::Extend(single, list)),
            None => single,
        })
    }

    /// `List.append_if_ok(list, result)`, or `List.prepend_if_ok`: the list with the payload of
    /// an `:Ok` added, or the list itself for an `:Err`, in a switch on the result's shape.
    unsafe fn add_if_ok(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, result] = arguments.exactly(2)?[..] else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        let (list, ty) = self.known_list(list, name)?;
        let result = self.value(result)?;
        let (ok, err) = (tag_layout("Ok", 1)?, tag_layout("Err", 1)?);
        let (candidates, exhaustive) = self.shapes_of(self.builder.ty(result), &[ok, err], &[])?;
        let appending = name == "List.append_if_ok";
        let value = self.switch_shapes(
            result,
            &candidates,
            exhaustive,
            Expected::Free,
            &mut |lowering, shape| match shape {
                Some(shape) if shapes::shape(shape).layout == ok => {
                    let payload = lowering.field(result, shape, 0)?;
                    if appending {
                        lowering.append_value(list, payload).map(Some)
                    } else {
                        lowering.prepend_value(Some((list, ty)), payload).map(Some)
                    }
                }
                Some(shape) if shapes::shape(shape).layout == err => Ok(Some(list)),
                _ => Ok(lowering.unreachable()),
            },
        )?;
        let value = value.ok_or(Refusal(Cow::Owned(format!("a `{name}` of no result"))))?;
        self.usable(value)?;
        Ok(value)
    }

    /// `List.set(list, index, value)`. An index outside the list leaves it unchanged.
    unsafe fn list_set(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, index, value] = arguments.exactly(3)?[..] else {
            return Err(Refusal::new("a malformed `List.set`"));
        };
        let (list, ty) = self.known_list(list, "List.set")?;
        let index = self.value(index)?;
        let index = self.to_i64(index)?;
        let value = self.value(value)?;
        self.usable(value)?;
        // A tag of another shape widens the item union, as an append does.
        let ty = self
            .builder
            .ty(value)
            .held_item()
            .and_then(|found| {
                joined(
                    ty,
                    Type::Vector {
                        item: found,
                        array: true,
                    },
                )
            })
            .ok_or(Refusal::new("a `List.set` of an item of another type"))?;
        let inside = self.within_list(list, index);
        let result = self.builder.local(ty);
        let (set, keep, join) = (
            self.builder.block(),
            self.builder.block(),
            self.builder.block(),
        );
        self.builder.terminate(Terminator::Branch {
            condition: inside,
            then: set,
            otherwise: keep,
        });
        self.builder.switch_to(set);
        let updated = self.builder.emit(ty, Op::Set(list, index, value));
        self.builder.push(Some(result), Op::Copy(updated));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(keep);
        self.builder.push(Some(result), Op::Copy(list));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(join);
        Ok(result)
    }

    /// Whether the 1-based `I64` `index` is a position of `list`.
    fn within_list(&mut self, list: Local, index: Local) -> Local {
        let one = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
        let count = self.builder.emit(Type::I64, Op::Length(list));
        let above = self
            .builder
            .emit(Type::Lgl, Op::Compare(Compare::Ge, Item::I64, index, one));
        let below = self
            .builder
            .emit(Type::Lgl, Op::Compare(Compare::Le, Item::I64, index, count));
        let both = self.builder.emit(Type::Lgl, Op::And(above, below));
        self.builder.emit(Type::Bool, Op::Truth(both))
    }

    /// `List.replace(list, index, value)` as `(list = ..., value = ...)`: the list with `value`
    /// at `index` and the item it replaced, or the list unchanged and `value` itself when
    /// `index` is not a position of the list.
    unsafe fn replace(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, index, value] = arguments.exactly(3)?[..] else {
            return Err(Refusal::new("a malformed `List.replace`"));
        };
        let (list, ty) = self.known_list(list, "List.replace")?;
        let item = ty.items().ok_or(Refusal::new(
            "a `List.replace` of something other than a ry list",
        ))?;
        let index = self.value(index)?;
        let index = self.to_i64(index)?;
        let value = self.value(value)?;
        self.usable(value)?;
        if self.builder.ty(value).held_item() != Some(item) {
            return Err(Refusal::new(
                "a `List.replace` with an item of another type",
            ));
        }
        let inside = self.within_list(list, index);
        let output = self.builder.local(ty);
        self.builder.push(Some(output), Op::Copy(list));
        let previous = self.builder.local(Type::of_item(item));
        self.builder.push(Some(previous), Op::Copy(value));
        let (set, join) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: inside,
            then: set,
            otherwise: join,
        });
        self.builder.switch_to(set);
        let old = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(list, index));
        self.builder.push(Some(previous), Op::Copy(old));
        let updated = self.builder.emit(ty, Op::Set(list, index, value));
        self.builder.push(Some(output), Op::Copy(updated));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(join);
        self.record_of(&[("list", output), ("value", previous)])
    }

    /// `List.update(list, index, f)`: the list with `f` of the item at `index` in its place, or
    /// the list unchanged when `index` is not a position of the list.
    unsafe fn list_update(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, index, function] = arguments.exactly(3)?[..] else {
            return Err(Refusal::new("a malformed `List.update`"));
        };
        let (list, ty) = self.known_list(list, "List.update")?;
        let item = ty.items().ok_or(Refusal::new(
            "a `List.update` of something other than a ry list",
        ))?;
        let index = self.value(index)?;
        let index = self.to_i64(index)?;
        let callable = self.function_argument(function)?;
        let inside = self.within_list(list, index);
        let output = self.builder.local(ty);
        self.builder.push(Some(output), Op::Copy(list));
        let (set, join) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: inside,
            then: set,
            otherwise: join,
        });
        self.builder.switch_to(set);
        let old = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(list, index));
        let value = self.apply(callable, &[old])?.ok_or(Refusal::new(
            "a `List.update` callback that never completes",
        ))?;
        self.usable(value)?;
        if self.builder.ty(value).held_item() != Some(item) {
            return Err(Refusal::new(
                "a `List.update` callback that gives an item of another type",
            ));
        }
        let updated = self.builder.emit(ty, Op::Set(list, index, value));
        self.builder.push(Some(output), Op::Copy(updated));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(join);
        Ok(output)
    }

    /// `List.split_at(list, count)` as `(before = ..., others = ...)`, the count clamped to the
    /// list. A negative count is an error, which the interpreter raises.
    unsafe fn split_at(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, count] = arguments.exactly(2)?[..] else {
            return Err(Refusal::new("a malformed `List.split_at`"));
        };
        let (list, ty) = self.known_list(list, "List.split_at")?;
        let count = self.value(count)?;
        let count = self.to_i64(count)?;
        let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
        let negative = self
            .builder
            .emit(Type::Lgl, Op::Compare(Compare::Lt, Item::I64, count, zero));
        self.fail_if(negative);
        let total = self.builder.emit(Type::I64, Op::Length(list));
        let taken = self
            .builder
            .emit(Type::I64, Op::Arith(Arith::Min, Item::I64, count, total));
        let rest = self
            .builder
            .emit(Type::I64, Op::Arith(Arith::Sub, Item::I64, total, taken));
        let before = self.builder.emit(ty, Op::Slice(list, zero, taken));
        let others = self.builder.emit(ty, Op::Slice(list, taken, rest));
        self.record_of(&[("before", before), ("others", others)])
    }

    /// `List.split_first(list, delimiter)`, or `List.split_last`, as
    /// `:Ok((before = ..., after = ...))` around the first (or last) item equal to `delimiter`,
    /// or `:Err(:NotFound)`. Items compare as the interpreter's `identical` does, which for
    /// whole numbers, strings and Bools is equality.
    unsafe fn split_around(&mut self, last: bool, arguments: &Arguments) -> Result<Local, Refusal> {
        let name = if last {
            "List.split_last"
        } else {
            "List.split_first"
        };
        let [list, delimiter] = arguments.exactly(2)?[..] else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        let (list, ty) = self.known_list(list, name)?;
        let Some(item @ (Item::I64 | Item::Str | Item::Lgl)) = ty.items() else {
            return Err(Refusal(Cow::Owned(format!(
                "a `{name}` of items other than whole numbers, strings and Bools"
            ))));
        };
        let delimiter = self.value(delimiter)?;
        self.usable(delimiter)?;
        // `identical` holds an `I64` item and an `F64` delimiter apart, which a comparison
        // that converts one side would not.
        if self.builder.ty(delimiter).held_item() != Some(item) {
            return Err(Refusal(Cow::Owned(format!(
                "a `{name}` with a delimiter of another type"
            ))));
        }
        let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
        let found = self.builder.local(Type::I64);
        self.builder.push(Some(found), Op::Copy(zero));
        let held = self.builder.emit(ty, Op::Copy(list));
        let past_end = last.then(|| self.past_end(held));
        let positions = self.positions(held);
        let position = self.position(past_end, positions.counter);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, position));
        let equal = self.identical_items(element, delimiter, name)?;
        let (hit, next) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: equal,
            then: hit,
            otherwise: next,
        });
        self.builder.switch_to(hit);
        self.builder.push(Some(found), Op::Copy(position));
        self.builder.terminate(Terminator::Jump(positions.exit));
        self.builder.switch_to(next);
        self.next_position(positions);
        let present = self.test(Compare::Gt, Item::I64, found, zero);
        let (split, missing) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: present,
            then: split,
            otherwise: missing,
        });
        self.builder.switch_to(split);
        let one = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
        let taken = self
            .builder
            .emit(Type::I64, Op::Arith(Arith::Sub, Item::I64, found, one));
        let total = self.builder.emit(Type::I64, Op::Length(held));
        let rest = self
            .builder
            .emit(Type::I64, Op::Arith(Arith::Sub, Item::I64, total, found));
        let before = self.builder.emit(ty, Op::Slice(held, zero, taken));
        let after = self.builder.emit(ty, Op::Slice(held, found, rest));
        let parts = self.record_of(&[("before", before), ("after", after)])?;
        let ok = self.tag("Ok", &[parts])?;
        let split_end = self.builder.current;
        self.builder.switch_to(missing);
        let reason = self.tag("NotFound", &[])?;
        let err = self.tag("Err", &[reason])?;
        self.joined_result(split_end, ok, err)
    }

    /// `List.split_on(list, delimiter)`: the pieces of the list between the items equal to
    /// `delimiter`, as slices pushed onto a list of lists. `List.split_on_list` splits on a run
    /// of items equal to the delimiter list's, and a match resumes the search after the run.
    /// Items compare as in [`Self::split_around`]; an empty delimiter list runs in the
    /// interpreter.
    unsafe fn split_on(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, delimiter] = arguments.exactly(2)?[..] else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        let (list, ty) = self.known_list(list, name)?;
        let Some(item @ (Item::I64 | Item::Str | Item::Lgl)) = ty.items() else {
            return Err(Refusal(Cow::Owned(format!(
                "a `{name}` of items other than whole numbers, strings and Bools"
            ))));
        };
        let delimiter = if name == "List.split_on_list" {
            let (delimiter, delimiter_ty) = self.known_list(delimiter, name)?;
            if delimiter_ty != ty {
                return Err(Refusal(Cow::Owned(format!(
                    "a `{name}` with a delimiter of another type"
                ))));
            }
            self.builder.emit(ty, Op::Copy(delimiter))
        } else {
            let delimiter = self.value(delimiter)?;
            self.usable(delimiter)?;
            if self.builder.ty(delimiter).held_item() != Some(item) {
                return Err(Refusal(Cow::Owned(format!(
                    "a `{name}` with a delimiter of another type"
                ))));
            }
            // A lone item is a run of one.
            let empty = self.empty_list(item);
            self.builder.emit(ty, Op::Push(empty, delimiter))
        };
        let held = self.builder.emit(ty, Op::Copy(list));
        let total = self.builder.emit(Type::I64, Op::Length(held));
        let run = self.builder.emit(Type::I64, Op::Length(delimiter));
        let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
        let one = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
        let no_run = self
            .builder
            .emit(Type::Lgl, Op::Compare(Compare::Eq, Item::I64, run, zero));
        self.fail_if(no_run);
        let pieces = self.empty_list(
            ty.held_item()
                .ok_or(Refusal::new("a split of something a list cannot hold"))?,
        );
        let pieces_ty = self.builder.ty(pieces);
        // `start` is where the current piece begins and `resume` the first position a run may
        // start at, both as 0-based offsets.
        let start = self.builder.emit(Type::I64, Op::Copy(zero));
        let resume = self.builder.emit(Type::I64, Op::Copy(zero));
        let last_start = self
            .builder
            .emit(Type::I64, Op::Arith(Arith::Sub, Item::I64, total, run));
        let positions = self.positions(held);
        let at = self.builder.emit(
            Type::I64,
            Op::Arith(Arith::Sub, Item::I64, positions.counter, one),
        );
        let resumed = self
            .builder
            .emit(Type::Lgl, Op::Compare(Compare::Ge, Item::I64, at, resume));
        let room = self.builder.emit(
            Type::Lgl,
            Op::Compare(Compare::Le, Item::I64, at, last_start),
        );
        let candidate = self.builder.emit(Type::Lgl, Op::And(resumed, room));
        let candidate = self.builder.emit(Type::Bool, Op::Truth(candidate));
        let (compare, next) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: candidate,
            then: compare,
            otherwise: next,
        });
        self.builder.switch_to(compare);
        let window = self.count_up(one, run);
        let offset = self.builder.emit(
            Type::I64,
            Op::Arith(Arith::Add, Item::I64, at, window.counter),
        );
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, offset));
        let wanted = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(delimiter, window.counter));
        let equal = self.identical_items(element, wanted, name)?;
        let (same, differs) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: equal,
            then: same,
            otherwise: differs,
        });
        self.builder.switch_to(differs);
        self.builder.terminate(Terminator::Jump(next));
        self.builder.switch_to(same);
        self.next_position(window);
        // Every item of the run matched: the piece before it ends here.
        let length = self
            .builder
            .emit(Type::I64, Op::Arith(Arith::Sub, Item::I64, at, start));
        let piece = self.builder.emit(ty, Op::Slice(held, start, length));
        let grown = self.builder.emit(pieces_ty, Op::Push(pieces, piece));
        self.builder.push(Some(pieces), Op::Copy(grown));
        let after = self
            .builder
            .emit(Type::I64, Op::Arith(Arith::Add, Item::I64, at, run));
        self.builder.push(Some(start), Op::Copy(after));
        self.builder.push(Some(resume), Op::Copy(after));
        self.builder.terminate(Terminator::Jump(next));
        self.builder.switch_to(next);
        self.next_position(positions);
        let rest = self
            .builder
            .emit(Type::I64, Op::Arith(Arith::Sub, Item::I64, total, start));
        let piece = self.builder.emit(ty, Op::Slice(held, start, rest));
        let grown = self.builder.emit(pieces_ty, Op::Push(pieces, piece));
        self.builder.push(Some(pieces), Op::Copy(grown));
        Ok(pieces)
    }

    /// `List.starts_with(list, prefix)`, or `List.ends_with(list, suffix)`, in a loop over the
    /// part that leaves at the first item that differs.
    unsafe fn affix(&mut self, at_start: bool, arguments: &Arguments) -> Result<Local, Refusal> {
        let name = if at_start {
            "List.starts_with"
        } else {
            "List.ends_with"
        };
        let [list, part] = arguments.exactly(2)?[..] else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        let (list, ty) = self.known_list(list, name)?;
        let (part, part_ty) = self.known_list(part, name)?;
        if ty != part_ty {
            return Err(Refusal(Cow::Owned(format!(
                "a `{name}` of lists of different item types"
            ))));
        }
        let item = match ty.items() {
            Some(item @ (Item::I64 | Item::Str | Item::Lgl)) => item,
            _ => {
                return Err(Refusal(Cow::Owned(format!(
                    "a `{name}` of items other than whole numbers, strings and Bools"
                ))));
            }
        };
        let result = self.builder.local(Type::Lgl);
        let yes = self.builder.emit(Type::Lgl, Op::Const(Constant::Lgl(1)));
        self.builder.push(Some(result), Op::Copy(yes));
        let total = self.builder.emit(Type::I64, Op::Length(list));
        let length = self.builder.emit(Type::I64, Op::Length(part));
        let longer = self.test(Compare::Gt, Item::I64, length, total);
        let (short, check, done) = (
            self.builder.block(),
            self.builder.block(),
            self.builder.block(),
        );
        self.builder.terminate(Terminator::Branch {
            condition: longer,
            then: short,
            otherwise: check,
        });
        self.builder.switch_to(short);
        let no = self.builder.emit(Type::Lgl, Op::Const(Constant::Lgl(0)));
        self.builder.push(Some(result), Op::Copy(no));
        self.builder.terminate(Terminator::Jump(done));
        self.builder.switch_to(check);
        let offset = if at_start {
            self.builder.emit(Type::I64, Op::Const(Constant::I64(0)))
        } else {
            self.builder
                .emit(Type::I64, Op::Arith(Arith::Sub, Item::I64, total, length))
        };
        let held = self.builder.emit(ty, Op::Copy(part));
        let positions = self.positions(held);
        let mine = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, positions.counter));
        let at = self.builder.emit(
            Type::I64,
            Op::Arith(Arith::Add, Item::I64, offset, positions.counter),
        );
        let theirs = self.builder.emit(Type::of_item(item), Op::ItemAt(list, at));
        let equal = self.identical_items(theirs, mine, name)?;
        let (differ, next) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: equal,
            then: next,
            otherwise: differ,
        });
        self.builder.switch_to(differ);
        let no = self.builder.emit(Type::Lgl, Op::Const(Constant::Lgl(0)));
        self.builder.push(Some(result), Op::Copy(no));
        self.builder.terminate(Terminator::Jump(positions.exit));
        self.builder.switch_to(next);
        self.next_position(positions);
        self.builder.terminate(Terminator::Jump(done));
        self.builder.switch_to(done);
        Ok(self.builder.emit(Type::Bool, Op::Truth(result)))
    }

    /// Whether `a` equals `b`, as a `Bool` to branch on, for the list function `name`.
    unsafe fn identical_items(&mut self, a: Local, b: Local, name: &str) -> Result<Local, Refusal> {
        // Two lists are one item each here, where `compare` would pair their items.
        let (ta, tb) = (self.builder.ty(a), self.builder.ty(b));
        if let (Some(item @ Item::Array(_)), Some(other)) = (ta.held_item(), tb.held_item()) {
            if item != other {
                return Err(Refusal(Cow::Owned(format!("a `{name}` of another type"))));
            }
            return Ok(self
                .builder
                .emit(Type::Bool, Op::Compare(Compare::Eq, item, a, b)));
        }
        let equal = self.compare(Compare::Eq, a, b)?;
        match self.builder.ty(equal) {
            Type::Bool => Ok(equal),
            Type::Lgl => Ok(self.builder.emit(Type::Bool, Op::Truth(equal))),
            _ => Err(Refusal(Cow::Owned(format!("a `{name}` of another type")))),
        }
    }

    /// `List.intersperse(list, separator)`: the items with `separator` pushed between each two.
    unsafe fn intersperse(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, separator] = arguments.exactly(2)?[..] else {
            return Err(Refusal::new("a malformed `List.intersperse`"));
        };
        let (list, ty) = self.known_list(list, "List.intersperse")?;
        let item = ty.items().ok_or(Refusal::new(
            "a `List.intersperse` of something other than a ry list",
        ))?;
        let separator = self.value(separator)?;
        self.usable(separator)?;
        if self.builder.ty(separator).held_item() != Some(item) {
            return Err(Refusal::new(
                "a `List.intersperse` with a separator of another type",
            ));
        }
        let output = self.empty_list(item);
        let held = self.builder.emit(ty, Op::Copy(list));
        let positions = self.positions(held);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, positions.counter));
        let one = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
        let later = self.test(Compare::Gt, Item::I64, positions.counter, one);
        let (spaced, join) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: later,
            then: spaced,
            otherwise: join,
        });
        self.builder.switch_to(spaced);
        let grown = self.builder.emit(ty, Op::Push(output, separator));
        self.builder.push(Some(output), Op::Copy(grown));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(join);
        let grown = self.builder.emit(ty, Op::Push(output, element));
        self.builder.push(Some(output), Op::Copy(grown));
        self.next_position(positions);
        Ok(output)
    }

    /// `List.join(lists)`: the items of each list in turn, extended onto a fresh list.
    unsafe fn join(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [lists] = arguments.exactly(1)?[..] else {
            return Err(Refusal::new("a malformed `List.join`"));
        };
        let (lists, ty) = self.known_list(lists, "List.join")?;
        let inner = ty.items().map(Type::of_item).ok_or(Refusal::new(
            "a `List.join` of something other than a ry list",
        ))?;
        let Type::Vector { item, array: true } = inner else {
            return Err(Refusal::new("a `List.join` of something other than lists"));
        };
        let output = self.empty_list(item);
        let held = self.builder.emit(ty, Op::Copy(lists));
        let positions = self.positions(held);
        let piece = self
            .builder
            .emit(inner, Op::ItemAt(held, positions.counter));
        let grown = self.builder.emit(inner, Op::Extend(output, piece));
        self.builder.push(Some(output), Op::Copy(grown));
        self.next_position(positions);
        Ok(output)
    }

    /// `List.for_each(list, f)`: `f` of each item for its effects, in a loop with `f` inlined.
    unsafe fn for_each(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, function] = arguments.exactly(2)?[..] else {
            return Err(Refusal::new("a malformed `List.for_each`"));
        };
        let (list, ty) = self.known_list(list, "List.for_each")?;
        let item = ty.items().ok_or(Refusal::new(
            "a `List.for_each` of something other than a ry list",
        ))?;
        let callable = self.function_argument(function)?;
        let held = self.builder.emit(ty, Op::Copy(list));
        let positions = self.positions(held);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, positions.counter));
        self.apply(callable, &[element])?.ok_or(Refusal::new(
            "a `List.for_each` callback that never completes",
        ))?;
        self.next_position(positions);
        Ok(self.builder.emit(Type::Null, Op::Const(Constant::Null)))
    }

    /// A record of `fields`, each named, as `(name = value, ...)` builds it.
    unsafe fn record_of(&mut self, fields: &[(&str, Local)]) -> Result<Local, Refusal> {
        let names: SmallVec<[Sexp; 4]> = fields
            .iter()
            .map(|&(name, _)| rvalues::install(name).map_err(|_| Refusal::new("a field name")))
            .collect::<Result<_, _>>()?;
        let (layout, slots) = interp::records::constructor_layout(None, &names, true)
            .ok_or(Refusal::new("a record the interpreter rejects"))?;
        let mut placed: SmallVec<[(usize, Local); 4]> = slots
            .iter()
            .copied()
            .zip(fields.iter().map(|&(_, value)| value))
            .collect();
        placed.sort_by_key(|&(slot, _)| slot);
        let values: SmallVec<[Local; 4]> = placed.iter().map(|&(_, value)| value).collect();
        self.boxed(layout, false, &values)
    }

    /// `List.drop_at(list, index)`: the other items, pushed in order onto a fresh list. An index
    /// outside the list keeps them all.
    unsafe fn drop_at(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, index] = arguments.exactly(2)?[..] else {
            return Err(Refusal::new("a malformed `List.drop_at`"));
        };
        let (list, ty) = self.known_list(list, "List.drop_at")?;
        let index = self.value(index)?;
        let index = self.to_i64(index)?;
        let item = ty.items().ok_or(Refusal::new(
            "a `List.drop_at` of something other than a ry list",
        ))?;
        let kept = self.empty_list(item);
        let held = self.builder.emit(ty, Op::Copy(list));
        let positions = self.positions(held);
        let other = self.test(Compare::Ne, Item::I64, positions.counter, index);
        let (keep, after) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: other,
            then: keep,
            otherwise: after,
        });
        self.builder.switch_to(keep);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, positions.counter));
        let grown = self.builder.emit(ty, Op::Push(kept, element));
        self.builder.push(Some(kept), Op::Copy(grown));
        self.builder.terminate(Terminator::Jump(after));
        self.builder.switch_to(after);
        self.next_position(positions);
        Ok(kept)
    }

    /// The `Bool` a predicate gives for `element`. The interpreter raises the error for an
    /// answer other than `TRUE` or `FALSE`.
    unsafe fn predicate(&mut self, callable: Callable, element: Local) -> Result<Local, Refusal> {
        let answer = self
            .apply(callable, &[element])?
            .ok_or(Refusal::new("a predicate that never completes"))?;
        match self.builder.ty(answer) {
            Type::Bool => Ok(answer),
            Type::Lgl => Ok(self.builder.emit(Type::Bool, Op::Truth(answer))),
            _ => Err(Refusal::new("a predicate whose answer is not a logical")),
        }
    }

    /// A fresh empty ry list of `item`s, in a variable the caller grows.
    fn empty_list(&mut self, item: Item) -> Local {
        let ty = Type::Vector { item, array: true };
        let zero = self.int(0);
        let fresh = self
            .builder
            .emit(Type::vector(item), Op::NewVector(item, zero));
        let empty = self.builder.emit(ty, Op::Relabel(fresh, ty));
        self.builder.emit(ty, Op::Copy(empty))
    }

    /// `List.keep_if(list, predicate)`: the items the predicate is true for, pushed in order
    /// onto a fresh list in a loop with the predicate inlined. `List.drop_if` keeps the others,
    /// and `List.keep_if_try` takes a predicate that gives `:Ok(answer)` and leaves with the first
    /// `:Err`.
    unsafe fn keep_if(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, predicate] = arguments.exactly(2)?[..] else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        let (list, ty) = self.known_list(list, name)?;
        let item = ty.items().ok_or(Refusal(Cow::Owned(format!(
            "a `{name}` of something other than a ry list"
        ))))?;
        let callable = self.function_argument(predicate)?;
        let trying = name == "List.keep_if_try";
        let kept = self.empty_list(item);
        let held = self.builder.emit(ty, Op::Copy(list));
        let positions = self.positions(held);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, positions.counter));
        let (answer, failure) = if trying {
            let result = self
                .apply(callable, &[element])?
                .ok_or(Refusal::new("a predicate that never completes"))?;
            let (answer, failure) = self.ok_or_leave(result)?;
            let answer = match self.builder.ty(answer) {
                Type::Bool => answer,
                Type::Lgl => self.builder.emit(Type::Bool, Op::Truth(answer)),
                _ => return Err(Refusal::new("a predicate whose answer is not a logical")),
            };
            (answer, failure)
        } else {
            (self.predicate(callable, element)?, None)
        };
        let (keep, after) = (self.builder.block(), self.builder.block());
        let (then, otherwise) = if name != "List.drop_if" {
            (keep, after)
        } else {
            (after, keep)
        };
        self.builder.terminate(Terminator::Branch {
            condition: answer,
            then,
            otherwise,
        });
        self.builder.switch_to(keep);
        let grown = self.builder.emit(ty, Op::Push(kept, element));
        self.builder.push(Some(kept), Op::Copy(grown));
        self.builder.terminate(Terminator::Jump(after));
        self.builder.switch_to(after);
        self.next_position(positions);
        if trying {
            return self.ok_or_failure(kept, failure);
        }
        Ok(kept)
    }

    /// `List.find_first(list, predicate)` as `:Ok(item)`, or `List.find_first_index` as
    /// `:Ok(position)`, for the first item the predicate is true for, and `:Err(:NotFound)`
    /// when there is none. The loop leaves at the first match. `List.find_last` and
    /// `List.find_last_index` search from the end.
    unsafe fn find_first(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, predicate] = arguments.exactly(2)?[..] else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        let (list, ty) = self.known_list(list, name)?;
        let item = ty.items().ok_or(Refusal(Cow::Owned(format!(
            "a `{name}` of something other than a ry list"
        ))))?;
        let callable = self.function_argument(predicate)?;
        let held = self.builder.emit(ty, Op::Copy(list));
        let past_end = name
            .starts_with("List.find_last")
            .then(|| self.past_end(held));
        let positions = self.positions(held);
        let position = self.position(past_end, positions.counter);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, position));
        let answer = self.predicate(callable, element)?;
        let (found, next) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: answer,
            then: found,
            otherwise: next,
        });
        self.builder.switch_to(found);
        let value = if name.ends_with("_index") {
            self.builder.emit(Type::I64, Op::Copy(position))
        } else {
            element
        };
        let ok = self.tag("Ok", &[value])?;
        self.builder.switch_to(next);
        self.next_position(positions);
        let reason = self.tag("NotFound", &[])?;
        let err = self.tag("Err", &[reason])?;
        self.joined_result(found, ok, err)
    }

    /// The result an `:Ok` built in the unterminated block `found` and an `:Err` built in the
    /// current block join into, read in the block after both.
    unsafe fn joined_result(
        &mut self,
        found: BlockId,
        ok: Local,
        err: Local,
    ) -> Result<Local, Refusal> {
        let Some(Type::Tag(union)) = joined(self.builder.ty(ok), self.builder.ty(err)) else {
            return Err(Refusal::new("a search result"));
        };
        let join = self.builder.block();
        let result = self.builder.local(Type::Tag(union));
        self.builder.push(Some(result), Op::Copy(err));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(found);
        self.builder.push(Some(result), Op::Copy(ok));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(join);
        Ok(result)
    }

    /// `List.contains(list, value)`, in a loop that leaves at the first equal item.
    unsafe fn contains(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, value] = arguments.exactly(2)?[..] else {
            return Err(Refusal::new("a malformed `List.contains`"));
        };
        let (list, ty) = self.known_list(list, "List.contains")?;
        let item = ty.items().ok_or(Refusal::new(
            "a `List.contains` of something other than a ry list",
        ))?;
        if !matches!(item, Item::I64 | Item::Dbl | Item::Str | Item::Lgl) {
            return Err(Refusal::new(
                "a `List.contains` of items that are not scalars",
            ));
        }
        let value = self.value(value)?;
        let result = self.builder.local(Type::Lgl);
        let no = self.builder.emit(Type::Lgl, Op::Const(Constant::Lgl(0)));
        self.builder.push(Some(result), Op::Copy(no));
        let held = self.builder.emit(ty, Op::Copy(list));
        let positions = self.positions(held);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, positions.counter));
        let equal = self.compare(Compare::Eq, element, value)?;
        let equal = match self.builder.ty(equal) {
            Type::Bool => equal,
            Type::Lgl => self.builder.emit(Type::Bool, Op::Truth(equal)),
            _ => return Err(Refusal::new("a `List.contains` of another type")),
        };
        let (found, next) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: equal,
            then: found,
            otherwise: next,
        });
        self.builder.switch_to(found);
        let yes = self.builder.emit(Type::Lgl, Op::Const(Constant::Lgl(1)));
        self.builder.push(Some(result), Op::Copy(yes));
        self.builder.terminate(Terminator::Jump(positions.exit));
        self.builder.switch_to(next);
        self.next_position(positions);
        Ok(self.builder.emit(Type::Bool, Op::Truth(result)))
    }

    /// `List.reverse(list)`, written back to front into a fresh list.
    unsafe fn reverse(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list] = arguments.exactly(1)?[..] else {
            return Err(Refusal::new("a malformed `List.reverse`"));
        };
        let (list, ty) = self.known_list(list, "List.reverse")?;
        let item = ty.items().ok_or(Refusal::new(
            "a `List.reverse` of something other than a ry list",
        ))?;
        let held = self.builder.emit(ty, Op::Copy(list));
        let count = self.builder.emit(Type::Int, Op::Length(held));
        let total = self.builder.emit(Type::I64, Op::Length(held));
        let fresh = self
            .builder
            .emit(Type::vector(item), Op::NewVector(item, count));
        let output = self.builder.local(ty);
        self.builder.push(Some(output), Op::Relabel(fresh, ty));
        let positions = self.positions(held);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, positions.counter));
        let one = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
        let after = self
            .builder
            .emit(Type::I64, Op::Arith(Arith::Add, Item::I64, total, one));
        let slot = self.builder.emit(
            Type::I64,
            Op::Arith(Arith::Sub, Item::I64, after, positions.counter),
        );
        let updated = self.builder.emit(ty, Op::Set(output, slot, element));
        self.builder.push(Some(output), Op::Copy(updated));
        self.next_position(positions);
        Ok(output)
    }

    /// `List.max(list)` or `List.min(list)` of numbers as `:Ok(extreme)`, or
    /// `:Err(:ListWasEmpty)`. An item replaces the extreme only when it compares strictly
    /// beyond it, as in the interpreter; a `NaN` makes the comparison `NA`, and the call then
    /// runs in the interpreter.
    unsafe fn list_extreme(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list] = arguments.exactly(1)?[..] else {
            return Err(Refusal::new("a malformed `List.max`"));
        };
        let (list, ty) = self.known_list(list, name)?;
        let item = ty.items().ok_or(Refusal::new(
            "a `List.max` of something other than a ry list",
        ))?;
        if !matches!(item, Item::I64 | Item::Dbl) {
            return Err(Refusal::new("a `List.max` of something other than numbers"));
        }
        let wanted = if name == "List.max" {
            Compare::Gt
        } else {
            Compare::Lt
        };
        let held = self.builder.emit(ty, Op::Copy(list));
        let total = self.builder.emit(Type::I64, Op::Length(held));
        let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
        let empty = self
            .builder
            .emit(Type::Bool, Op::Compare(Compare::Eq, Item::I64, total, zero));
        let (absent, present) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: empty,
            then: absent,
            otherwise: present,
        });
        self.builder.switch_to(present);
        let one = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
        let first = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, one));
        let best = self.builder.emit(Type::of_item(item), Op::Copy(first));
        let two = self.builder.emit(Type::I64, Op::Const(Constant::I64(2)));
        let positions = self.count_up(two, total);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, positions.counter));
        let beyond = self
            .builder
            .emit(Type::Lgl, Op::Compare(wanted, item, element, best));
        let beyond = self.builder.emit(Type::Bool, Op::Truth(beyond));
        let (replace, after) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: beyond,
            then: replace,
            otherwise: after,
        });
        self.builder.switch_to(replace);
        self.builder.push(Some(best), Op::Copy(element));
        self.builder.terminate(Terminator::Jump(after));
        self.builder.switch_to(after);
        self.next_position(positions);
        let ok = self.tag("Ok", &[best])?;
        let found = self.builder.current;
        self.builder.switch_to(absent);
        let reason = self.tag("ListWasEmpty", &[])?;
        let err = self.tag("Err", &[reason])?;
        self.joined_result(found, ok, err)
    }

    /// `List.join_map(list, f)`: the lists `f` gives for each item, concatenated in a loop with
    /// `f` inlined. An empty result stops the native code unless a `.ry_items` settles it,
    /// because the interpreter builds it without an item type, as for `List.map`.
    unsafe fn join_map(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let settled = std::mem::take(&mut self.settled_empty);
        let [list, function] = arguments.exactly(2)?[..] else {
            return Err(Refusal::new("a malformed `List.join_map`"));
        };
        let (list, ty) = self.known_list(list, "List.join_map")?;
        let item = ty.items().ok_or(Refusal::new(
            "a `List.join_map` of something other than a ry list",
        ))?;
        let callable = self.function_argument(function)?;
        let held = self.builder.emit(ty, Op::Copy(list));
        let preheader = self.builder.current;
        // The result's type is known only once the body is lowered, so the empty result is
        // made in the preheader afterwards.
        let output = self.builder.local(Type::Null);
        let positions = self.positions(held);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, positions.counter));
        let piece = self
            .apply(callable, &[element])?
            .ok_or(Refusal::new("a mapped function that never completes"))?;
        let output_ty = self.builder.ty(piece);
        let Type::Vector {
            item: output_item,
            array: true,
        } = output_ty
        else {
            return Err(Refusal::new(
                "a `List.join_map` whose function gives no ry list",
            ));
        };
        self.builder.function.locals[output.0 as usize] = output_ty;
        let grown = self.builder.emit(output_ty, Op::Extend(output, piece));
        self.builder.push(Some(output), Op::Copy(grown));
        self.next_position(positions);
        let exit = self.builder.current;
        self.builder.switch_to(preheader);
        let empty = self.empty_list(output_item);
        self.builder.push(Some(output), Op::Copy(empty));
        self.builder.switch_to(exit);
        if !settled {
            let length = self.builder.emit(Type::Int, Op::Length(output));
            let zero = self.int(0);
            let nothing = self.builder.emit(Type::Bool, Op::Same(length, zero));
            self.fail_if(nothing);
        }
        Ok(output)
    }

    /// `List.chunks_of(list, size)`: slices of `size` items, the last one shorter. A size below
    /// one runs in the interpreter, which gives no chunks for zero and raises the error for a
    /// negative size.
    unsafe fn chunks_of(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list, size] = arguments.exactly(2)?[..] else {
            return Err(Refusal::new("a malformed `List.chunks_of`"));
        };
        let (list, ty) = self.known_list(list, "List.chunks_of")?;
        let size = self.value(size)?;
        let size = self.to_i64(size)?;
        let one = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
        let below = self
            .builder
            .emit(Type::Lgl, Op::Compare(Compare::Lt, Item::I64, size, one));
        self.fail_if(below);
        let item = ty.held_item().ok_or(Refusal::new(
            "a `List.chunks_of` of something a list cannot hold",
        ))?;
        let chunks = self.empty_list(item);
        let chunks_ty = self.builder.ty(chunks);
        let held = self.builder.emit(ty, Op::Copy(list));
        let total = self.builder.emit(Type::I64, Op::Length(held));
        let arith = |lower: &mut Self, op: Arith, a: Local, b: Local| {
            lower
                .builder
                .emit(Type::I64, Op::Arith(op, Item::I64, a, b))
        };
        let spare = arith(self, Arith::Sub, size, one);
        let padded = arith(self, Arith::Add, total, spare);
        let count = arith(self, Arith::IntDiv, padded, size);
        let positions = self.count_up(one, count);
        let before = arith(self, Arith::Sub, positions.counter, one);
        let start = arith(self, Arith::Mul, before, size);
        let rest = arith(self, Arith::Sub, total, start);
        let length = arith(self, Arith::Min, size, rest);
        let chunk = self.builder.emit(ty, Op::Slice(held, start, length));
        let grown = self.builder.emit(chunks_ty, Op::Push(chunks, chunk));
        self.builder.push(Some(chunks), Op::Copy(grown));
        self.next_position(positions);
        Ok(chunks)
    }

    /// `List.map2(a, b, f)`, `List.map3(a, b, c, f)` and `List.map4`: `f` of the items at each
    /// position up to the shortest list's length, in a loop with `f` inlined. `List.map_try(list,
    /// f)` maps one list with an `f` that gives results, leaves with the first `:Err` and wraps
    /// the list in `:Ok`. An empty result stops the native code unless a `.ry_items` settles it,
    /// because the interpreter builds it without an item type, as for `List.map`.
    unsafe fn map_many(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let settled = std::mem::take(&mut self.settled_empty);
        let trying = name == "List.map_try";
        let parameters = arguments.exactly(match name {
            "List.map_try" => 2,
            "List.map3" => 4,
            "List.map4" => 5,
            _ => 3,
        })?;
        let Some((&function, lists)) = parameters.split_last() else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        let mut held: SmallVec<[(Local, Item); 3]> = SmallVec::new();
        for &list in lists {
            let (list, ty) = self.known_list(list, name)?;
            let item = ty
                .items()
                .ok_or(Refusal::new("a map of something other than a ry list"))?;
            held.push((self.builder.emit(ty, Op::Copy(list)), item));
        }
        let callable = self.function_argument(function)?;
        let total = self.builder.emit(Type::I64, Op::Length(held[0].0));
        for &(list, _) in &held[1..] {
            let length = self.builder.emit(Type::I64, Op::Length(list));
            let shorter = self
                .builder
                .emit(Type::I64, Op::Arith(Arith::Min, Item::I64, total, length));
            self.builder.push(Some(total), Op::Copy(shorter));
        }
        let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
        if !settled {
            let empty = self
                .builder
                .emit(Type::Lgl, Op::Compare(Compare::Eq, Item::I64, total, zero));
            self.fail_if(empty);
        }
        let count = self.builder.emit(Type::Int, Op::CountOfI64(total));
        let preheader = self.builder.current;
        let output = self.builder.local(Type::Null);
        let one = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
        let positions = self.count_up(one, total);
        let mut elements: SmallVec<[Local; 3]> = SmallVec::new();
        for &(list, item) in &held {
            elements.push(
                self.builder
                    .emit(Type::of_item(item), Op::ItemAt(list, positions.counter)),
            );
        }
        let value = self
            .apply(callable, &elements)?
            .ok_or(Refusal::new("a mapped function that never completes"))?;
        let (value, failure) = if trying {
            self.ok_or_leave(value)?
        } else {
            (value, None)
        };
        let value = if self.builder.ty(value) == Type::Lgl {
            self.builder.emit(Type::Bool, Op::Truth(value))
        } else {
            value
        };
        let result_item = self.builder.ty(value).held_item().ok_or(Refusal::new(
            "a mapped function whose value a list cannot hold",
        ))?;
        if matches!(result_item, Item::Int | Item::Lgl) && self.builder.ty(value) != Type::Bool {
            return Err(Refusal(Cow::Owned(format!(
                "a `{name}` to a list of R scalars"
            ))));
        }
        let result_item = if self.builder.ty(value) == Type::Bool {
            Item::Lgl
        } else {
            result_item
        };
        let output_ty = Type::Vector {
            item: result_item,
            array: true,
        };
        self.builder.function.locals[output.0 as usize] = output_ty;
        let updated = self
            .builder
            .emit(output_ty, Op::Set(output, positions.counter, value));
        self.builder.push(Some(output), Op::Copy(updated));
        self.next_position(positions);
        let exit = self.builder.current;
        self.builder.switch_to(preheader);
        let fresh = self
            .builder
            .emit(Type::vector(result_item), Op::NewVector(result_item, count));
        self.builder
            .push(Some(output), Op::Relabel(fresh, output_ty));
        self.builder.switch_to(exit);
        if trying {
            return self.ok_or_failure(output, failure);
        }
        Ok(output)
    }

    /// The count of the true items of `list`, a logical list.
    fn count_true(&mut self, list: Local) -> Local {
        let count = self.builder.local(Type::I64);
        self.builder.push(
            None,
            Op::CountTrue {
                program: SmallVec::new(),
                inputs: SmallVec::from_slice(&[list]),
                counts: SmallVec::from_slice(&[(0, count)]),
            },
        );
        count
    }

    /// Whether `body`, a predicate of the one parameter `item` over a `List<F64>`, only compares
    /// the item with `F64` literals and locals, and joins such comparisons with `&`, `|`, `!`
    /// and parentheses.
    unsafe fn compares_item(&self, body: Sexp, item: Sexp) -> bool {
        if body.kind() != SexpType::Language || body.car().kind() != SexpType::Symbol {
            return false;
        }
        let arguments = rvalues::pairlist_items(body.cdr());
        if arguments.iter().any(|(tag, _)| tag.kind() != SexpType::Nil) {
            return false;
        }
        let number = |operand: Sexp| match operand.kind() {
            SexpType::Real => rvalues::reals(operand).len() == 1,
            SexpType::Symbol => {
                operand != item
                    && matches!(
                        self.lookup(operand),
                        Some(Binding::Value(local) | Binding::Maybe(local))
                            if self.builder.ty(local) == Type::Dbl
                    )
            }
            _ => false,
        };
        match (&*rvalues::symbol_name(body.car()), &arguments[..]) {
            ("==" | "!=" | "<" | "<=" | ">" | ">=", &[(_, a), (_, b)]) => {
                (a == item && number(b)) || (number(a) && b == item)
            }
            ("&" | "|", &[(_, a), (_, b)]) => {
                self.compares_item(a, item) && self.compares_item(b, item)
            }
            ("!" | "(", &[(_, a)]) => self.compares_item(a, item),
            _ => false,
        }
    }

    /// A function argument: a literal or a local function, which inlines, or a typed closure,
    /// which is called.
    unsafe fn function_argument(&mut self, function: Sexp) -> Result<Callable, Refusal> {
        if function.kind() == SexpType::Symbol {
            match self.lookup(function) {
                Some(Binding::Function(callable)) => return Ok(callable),
                Some(Binding::Hidden) => return Err(hidden_refusal()),
                Some(Binding::Value(_) | Binding::Maybe(_)) => {
                    return Err(Refusal::new("a mapped function held in a variable"));
                }
                None => {}
            }
            let speculation = find_function(function, self.env)
                .ok_or(Refusal::new("a mapped function that does not resolve"))?;
            let found = speculation.function;
            if is_base_function(&speculation) {
                self.speculations.push(speculation);
                return Ok(Callable::Library(function));
            }
            if found.kind() != SexpType::Closure || !interp::annotations::is_typed(found) {
                return Err(Refusal::new(
                    "a mapped function that is not a typed closure",
                ));
            }
            self.speculations.push(speculation);
            return Ok(Callable::Typed(function, found));
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
        values: &[Local],
    ) -> Result<Option<Local>, Refusal> {
        let arguments: SmallVec<[Argument; 4]> =
            values.iter().map(|&value| Argument::Value(value)).collect();
        self.apply_arguments(callable, &arguments)
    }

    /// Calls a function argument with these arguments, which may pass functions in turn.
    unsafe fn apply_arguments(
        &mut self,
        callable: Callable,
        arguments: &[Argument],
    ) -> Result<Option<Local>, Refusal> {
        match callable {
            Callable::Inline(formals, body, scope) => {
                let arguments = self.positional_arguments(formals, body, scope + 1, arguments)?;
                self.inline(formals, body, scope + 1, &arguments, inline_storage)
            }
            Callable::Captured(formals, body, capture) => {
                // The function sees its captured names and its own, so it lowers with the scopes
                // in view swapped for one scope of those names.
                let mut scope = self.new_scope();
                scope.names = self.captures[capture as usize].clone();
                let held = std::mem::replace(&mut self.scopes, vec![scope]);
                let value = self
                    .positional_arguments(formals, body, 1, arguments)
                    .and_then(|arguments| {
                        self.inline(formals, body, 1, &arguments, inline_storage)
                    });
                self.scopes = held;
                value
            }
            Callable::Typed(name, function) => {
                self.typed_function(name, function, arguments, false)
            }
            Callable::Library(name) => {
                let (formals, body) = eta(name, arguments.len())?;
                self.inline(formals, body, 0, arguments, inline_storage)
            }
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
        match find_function(expression.car(), self.env) {
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
            let constant = Constant::Object(shape);
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
                        .collect::<SmallVec<[u32; 4]>>();
                    let any_tag = self.any_tag.get_or_insert_with(|| AnyTag {
                        epoch: shapes::epoch(),
                        ..AnyTag::default()
                    });
                    for (into, from) in [
                        (&mut any_tag.layouts, layouts),
                        (&mut any_tag.tags, tags),
                        (&mut any_tag.tested, &found[..]),
                    ] {
                        for &id in from {
                            if !into.contains(&id) {
                                into.push(id);
                            }
                        }
                    }
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
    /// `expected` says what the switch's value must be. An arm for a shape only confirmed
    /// somewhere whose value cannot be that gives up for that shape alone, as an arm that does
    /// not lower does. A tag from R is any tag, whatever the annotation says, so ill-typed R code
    /// can confirm `:One(F64)` beside the `:One(I64)` a function declares, and without a target
    /// the arms would disagree and the whole function would refuse for the rest of the session.
    /// When the box may have other shapes, a value nothing types is lowered again against the type
    /// a stray shape explains ([`stray_target`]); a dropped one only when a name its arms set
    /// would not join otherwise.
    unsafe fn switch_shapes(
        &mut self,
        value: Local,
        shapes: &[u32],
        exhaustive: bool,
        expected: Expected,
        arm: &mut dyn FnMut(&mut Self, Option<u32>) -> Result<Option<Local>, Refusal>,
    ) -> Result<Option<Local>, Refusal> {
        let mut cases: SmallVec<[Option<u32>; 4]> =
            shapes.iter().map(|&shape| Some(shape)).collect();
        if !exhaustive {
            cases.push(None);
        }
        let start = self.builder.current;
        let start_insts = self
            .builder
            .function
            .blocks
            .get(start.0 as usize)
            .map_or(0, |block| block.insts.len());
        let before = (matches!(expected, Expected::Free | Expected::Dropped(_)) && !exhaustive)
            .then(|| self.checkpoint());
        let shape_of = self.builder.emit(Type::Int, Op::ShapeOf(value));
        let mut ends: SmallVec<[(BlockId, Local); 4]> = SmallVec::new();
        let mut completed_cases: SmallVec<[Option<u32>; 4]> = SmallVec::new();
        let mut names: SmallVec<[(BlockId, Created); 4]> = SmallVec::new();
        let mut test = self.builder.current;
        for (position, &case) in cases.iter().enumerate() {
            self.builder.switch_to(test);
            let body = self.builder.block();
            match case {
                Some(shape) if position + 1 < cases.len() => {
                    let expected = self.shape_id(shape);
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
            let region = self.enter_control();
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
                (Ok(Some(completed)), checkpoint)
                    if matches!(expected, Expected::Declared(_) | Expected::Stray(_)) =>
                {
                    match (self.toward(completed, expected), checkpoint) {
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
            let created = self.leave_control(region);
            if let Some(completed) = completed? {
                if !matches!(expected, Expected::Dropped(_)) {
                    self.usable(completed)?;
                }
                ends.push((self.builder.current, completed));
                names.push((self.builder.current, created));
                completed_cases.push(case);
            }
        }
        if cases.is_empty() {
            self.builder.terminate(Terminator::Fail);
        }
        if let Some(before) = before {
            let types: SmallVec<[Type; 4]> = ends
                .iter()
                .map(|&(_, completed)| self.builder.ty(completed))
                .collect();
            // A dropped value's arms may disagree, but a name they set must still join.
            let restarts = match expected {
                Expected::Dropped(call) => self.drops_a_name(&names, call),
                _ => true,
            };
            if restarts
                && let Some(target) = stray_target(&completed_cases, &types)
                && self.restart(before, start, start_insts)
            {
                return self.switch_shapes(value, shapes, exhaustive, Expected::Stray(target), arm);
            }
        }
        Ok(self.join_arms(&ends, &names))
    }

    /// Joins the values the arms of a switch or `match` complete with, each with the end block
    /// it completes in, and the names each set, leaving the builder after them. `None` when no
    /// arm completes.
    fn join_arms(
        &mut self,
        ends: &[(BlockId, Local)],
        names: &[(BlockId, Created)],
    ) -> Option<Local> {
        self.join_names(names);
        let join = self.builder.block();
        let mut ty: Option<Option<Type>> = None;
        for &(_, completed) in ends {
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
                for &(end, completed) in ends {
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
        result
    }

    /// Forgets everything lowered since `checkpoint`, taken when block `start` held
    /// `start_insts` instructions, to lower the same code again from there. `false` when a block
    /// that existed before points into the code lowered since, so it cannot be forgotten.
    fn restart(&mut self, checkpoint: Checkpoint, start: BlockId, start_insts: usize) -> bool {
        let blocks = &mut self.builder.function.blocks;
        let kept = checkpoint.blocks;
        if blocks.iter().take(kept).enumerate().any(|(index, block)| {
            index != start.0 as usize
                && block
                    .terminator
                    .successors()
                    .iter()
                    .any(|target| target.0 as usize >= kept)
        }) {
            return false;
        }
        blocks.truncate(kept);
        let Some(entry) = blocks.get_mut(start.0 as usize) else {
            return false;
        };
        entry.insts.truncate(start_insts);
        entry.terminator = Terminator::Fail;
        self.builder.current = start;
        self.restore(checkpoint);
        true
    }

    /// `value` as a value of the type `expected` names, converting a number to the `I64` a
    /// function declares as its exit does, or `None` when it cannot be one.
    unsafe fn toward(&mut self, value: Local, expected: Expected) -> Option<Local> {
        let target = match expected {
            Expected::Declared(target) => target,
            Expected::Stray(target) => {
                return joined(target, self.builder.ty(value)).map(|_| value);
            }
            Expected::Free | Expected::Dropped(_) => return Some(value),
        };
        let value = match (target, self.builder.ty(value)) {
            (Type::I64, Type::Int | Type::Dbl) => self.to_i64(value).ok()?,
            _ => value,
        };
        joined(target, self.builder.ty(value)).map(|_| value)
    }

    fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            scopes: self.scopes.clone(),
            variables: self.variables.clone(),
            loops: self.loops.len(),
            inlined: self.inlined.clone(),
            speculations: self.speculations.len(),
            nesting: self.nesting,
            control: self.control,
            regions: self.regions.len(),
            unusable: self.unusable.len(),
            returned: self.returned,
            constructed: self.constructed.len(),
            r_calls: self.r_calls.len(),
            blocks: self.builder.function.blocks.len(),
            self_inlines: self.self_inlines,
            pending: self.pending.clone(),
            param_reads: self.param_reads.len(),
            reassigned: self.reassigned.len(),
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
        self.restore(checkpoint);
        true
    }

    fn restore(&mut self, checkpoint: Checkpoint) {
        self.scopes = checkpoint.scopes;
        self.variables = checkpoint.variables;
        self.loops.truncate(checkpoint.loops);
        self.inlined = checkpoint.inlined;
        self.speculations.truncate(checkpoint.speculations);
        self.nesting = checkpoint.nesting;
        self.control = checkpoint.control;
        self.regions.truncate(checkpoint.regions);
        self.unusable.truncate(checkpoint.unusable);
        self.returned = checkpoint.returned;
        self.constructed.truncate(checkpoint.constructed);
        self.r_calls.truncate(checkpoint.r_calls);
        self.self_inlines = checkpoint.self_inlines;
        self.param_reads.truncate(checkpoint.param_reads);
        self.reassigned.truncate(checkpoint.reassigned);
        // A list the forgotten code settled is pending again, with its placeholder item type.
        for family in &checkpoint.pending {
            let settled = family
                .first()
                .is_some_and(|first| !self.pending.iter().any(|current| current.contains(first)));
            if settled {
                retype_family(&mut self.builder.function, family, Item::Tag(shapes::ANY));
            }
        }
        self.pending = checkpoint.pending;
    }

    /// `match (value) { pattern => body ... }`. Over a record or tag the arms are decided for each
    /// shape the value may have, as the interpreter would decide them for a value of that shape;
    /// over any other value they are tested in order. `dropped` says the program drops the
    /// `match`'s value, as a statement's.
    unsafe fn match_expression(
        &mut self,
        call: Sexp,
        dropped: bool,
    ) -> Result<Option<Local>, Refusal> {
        let items = rvalues::pairlist_items(call.cdr());
        let Some((&(scrutinee_tag, scrutinee), arms)) = items.split_first() else {
            return Err(Refusal::new("a malformed `match`"));
        };
        if scrutinee_tag != rvalues::nil() {
            return Err(Refusal::new("a `match` with named arguments"));
        }
        let arrow = rvalues::install("=>").map_err(|_| Refusal::new("a malformed `match`"))?;
        let mut parsed: SmallVec<[MatchArm; 4]> = SmallVec::new();
        for &(tag, arm) in arms {
            if tag != rvalues::nil() || arm.kind() != SexpType::Language || arm.car() != arrow {
                return Err(Refusal::new("a malformed `match` arm"));
            }
            let parts = rvalues::pairlist_items(arm.cdr());
            if parts.iter().any(|&(tag, _)| tag != rvalues::nil()) {
                return Err(Refusal::new("a malformed `match` arm"));
            }
            let arm = match parts[..] {
                [(_, pattern), (_, body)] => MatchArm {
                    pattern,
                    guard: None,
                    body,
                },
                [(_, pattern), (_, guard), (_, body)] => {
                    // The interpreter evaluates a guard in a scratch environment, so what it
                    // assigns is gone afterwards, which the lowering does not model.
                    if writes(guard) {
                        return Err(Refusal::new("a `match` guard that assigns a name"));
                    }
                    MatchArm {
                        pattern,
                        guard: Some(guard),
                        body,
                    }
                }
                _ => return Err(Refusal::new("a malformed `match` arm")),
            };
            parsed.push(arm);
        }
        let value = self.value(scrutinee)?;
        let tail = self.inlined.is_empty() && self.tails.contains(&call);
        let expected = match self.declared {
            Some(declared) if tail => Expected::Declared(declared),
            _ if dropped => Expected::Dropped(call),
            _ => Expected::Free,
        };
        let ty = self.builder.ty(value);
        if !matches!(ty, Type::Record(_) | Type::Tag(_)) {
            return self.match_arms(value, Known::Typed, &parsed, dropped, expected);
        }
        let (mut layouts, mut tags): (SmallVec<[u32; 4]>, SmallVec<[u32; 4]>) = Default::default();
        for arm in &parsed {
            pattern_layouts(arm.pattern, &mut layouts, &mut tags)?;
        }
        let (candidates, exhaustive) = self.shapes_of(ty, &layouts, &tags)?;
        self.switch_shapes(
            value,
            &candidates,
            exhaustive,
            expected,
            &mut |lowering, shape| {
                let known = shape.map_or(Known::Other, Known::Shape);
                lowering.match_arms(value, known, &parsed, dropped, expected)
            },
        )
    }

    /// Lowers `arms` in order against `value`: each arm's tests and guard jump to the next arm
    /// when they fail, and the call fails when no arm matches, where the interpreter raises its
    /// error. Joins the values of the arms that complete.
    unsafe fn match_arms(
        &mut self,
        value: Local,
        known: Known,
        arms: &[MatchArm],
        dropped: bool,
        expected: Expected,
    ) -> Result<Option<Local>, Refusal> {
        let mut ends: SmallVec<[(BlockId, Local); 4]> = SmallVec::new();
        let mut names: SmallVec<[(BlockId, Created); 4]> = SmallVec::new();
        for arm in arms {
            let next = self.builder.block();
            let mut bindings: SmallVec<[(Sexp, Local); 4]> = SmallVec::new();
            let mut test = self.test_pattern(arm.pattern, value, known, next, &mut bindings)?;
            if test == Test::Never {
                self.builder.switch_to(next);
                continue;
            }
            if let Some(guard) = arm.guard {
                let region = self.enter_control();
                for &(symbol, bound) in &bindings {
                    self.bind(symbol, Binding::Value(bound));
                }
                let holds = self.condition(guard);
                self.leave_control(region);
                self.pass_if(holds?, next);
                test = Test::Maybe;
            }
            let region = self.enter_control();
            for &(symbol, bound) in &bindings {
                self.assign(symbol, bound)?;
            }
            let completed = self
                .part(arm.body, dropped)?
                .map(|completed| self.toward(completed, expected).unwrap_or(completed));
            let created = self.leave_control(region);
            if let Some(completed) = completed {
                if !matches!(expected, Expected::Dropped(_)) {
                    self.usable(completed)?;
                }
                ends.push((self.builder.current, completed));
                names.push((self.builder.current, created));
            }
            self.builder.switch_to(next);
            if test == Test::Always {
                break;
            }
        }
        self.builder.terminate(Terminator::Fail);
        Ok(self.join_arms(&ends, &names))
    }

    /// Lowers the tests of `pattern` against `value`, which jump to `fail` where it does not
    /// match, and adds the names it binds with their values.
    unsafe fn test_pattern(
        &mut self,
        pattern: Sexp,
        value: Local,
        known: Known,
        fail: BlockId,
        bindings: &mut SmallVec<[(Sexp, Local); 4]>,
    ) -> Result<Test, Refusal> {
        let unknown = || Refusal::new("a `match` pattern the typed core does not know");
        match pattern.kind() {
            SexpType::Symbol => {
                let name = rvalues::symbol_name(pattern);
                match &*name {
                    "_" => Ok(Test::Always),
                    "T" | "F" | "" => Err(unknown()),
                    _ if name.starts_with("..") => Err(unknown()),
                    _ => {
                        bindings.push((pattern, value));
                        Ok(Test::Always)
                    }
                }
            }
            SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::String
            | SexpType::I64 => self.test_literal(pattern, value, fail),
            SexpType::Language if pattern.car().kind() == SexpType::Symbol => {
                let head = rvalues::symbol_name(pattern.car());
                let items = rvalues::pairlist_items(pattern.cdr());
                match (&*head, &items[..]) {
                    ("(", &[(_, inner)]) => self.test_pattern(inner, value, known, fail, bindings),
                    ("|", &[(_, left), (_, right)]) => {
                        self.test_alternatives(left, right, value, known, fail, bindings)
                    }
                    (".ry_as", &[(_, inner), (_, name)]) if name.kind() == SexpType::Symbol => {
                        let test = self.test_pattern(inner, value, known, fail, bindings)?;
                        if test != Test::Never {
                            bindings.push((name, value));
                        }
                        Ok(test)
                    }
                    (".ry_array", _) => self.test_list(&items, value, fail, bindings),
                    (".ry_tag" | ".ry_record" | ".ry_tuple", _) => {
                        self.test_constructor(pattern, value, known, fail, bindings)
                    }
                    _ => Err(unknown()),
                }
            }
            _ => Err(unknown()),
        }
    }

    /// `left | right`: `right` is tested where `left` fails, and a name both bind joins.
    unsafe fn test_alternatives(
        &mut self,
        left: Sexp,
        right: Sexp,
        value: Local,
        known: Known,
        fail: BlockId,
        bindings: &mut SmallVec<[(Sexp, Local); 4]>,
    ) -> Result<Test, Refusal> {
        let right_start = self.builder.block();
        let mut left_bound: SmallVec<[(Sexp, Local); 4]> = SmallVec::new();
        let left_test = self.test_pattern(left, value, known, right_start, &mut left_bound)?;
        if left_test == Test::Always {
            bindings.extend(left_bound);
            return Ok(Test::Always);
        }
        let left_end = (left_test != Test::Never).then_some(self.builder.current);
        self.builder.switch_to(right_start);
        let mut right_bound: SmallVec<[(Sexp, Local); 4]> = SmallVec::new();
        let right_test = self.test_pattern(right, value, known, fail, &mut right_bound)?;
        let right_end = (right_test != Test::Never).then_some(self.builder.current);
        let (left_end, right_end) = match (left_end, right_end) {
            (None, None) => return Ok(Test::Never),
            (Some(left_end), None) => {
                self.builder.switch_to(left_end);
                bindings.extend(left_bound);
                return Ok(Test::Maybe);
            }
            (None, Some(_)) => {
                bindings.extend(right_bound);
                return Ok(right_test);
            }
            (Some(left_end), Some(right_end)) => (left_end, right_end),
        };
        let last = |bound: &[(Sexp, Local)], symbol: Sexp| {
            bound
                .iter()
                .rev()
                .find(|&&(name, _)| name == symbol)
                .map(|&(_, local)| local)
        };
        let differ = || Refusal::new("an alternative pattern whose sides bind different names");
        if right_bound
            .iter()
            .any(|&(symbol, _)| last(&left_bound, symbol).is_none())
        {
            return Err(differ());
        }
        let mut joined_bound: SmallVec<[(Sexp, Local); 4]> = SmallVec::new();
        for &(symbol, _) in &left_bound {
            if joined_bound.iter().any(|&(name, _)| name == symbol) {
                continue;
            }
            let (Some(from_left), Some(from_right)) =
                (last(&left_bound, symbol), last(&right_bound, symbol))
            else {
                return Err(differ());
            };
            if from_left == from_right {
                joined_bound.push((symbol, from_left));
                continue;
            }
            let ty = joined(self.builder.ty(from_left), self.builder.ty(from_right)).ok_or(
                Refusal::new("an alternative pattern binding a name to two types"),
            )?;
            let variable = self.builder.local(ty);
            for (end, local) in [(left_end, from_left), (right_end, from_right)] {
                self.builder.switch_to(end);
                self.builder.push(Some(variable), Op::Copy(local));
            }
            joined_bound.push((symbol, variable));
        }
        let matched = self.builder.block();
        for end in [left_end, right_end] {
            self.builder.switch_to(end);
            self.builder.terminate(Terminator::Jump(matched));
        }
        self.builder.switch_to(matched);
        bindings.extend(joined_bound);
        Ok(Test::Maybe)
    }

    /// A literal pattern, which matches a value of its kind that equals it, as
    /// `interp::records::literal_matches` decides: numbers match numbers whatever their storage,
    /// and a logical, a number and a string never match each other.
    unsafe fn test_literal(
        &mut self,
        pattern: Sexp,
        value: Local,
        fail: BlockId,
    ) -> Result<Test, Refusal> {
        if pattern.attrib() != rvalues::nil() || rvalues::length(pattern) != 1 {
            return Err(Refusal::new("a literal pattern that is not one value"));
        }
        let missing = || Refusal::new("a missing value as a pattern");
        let number = match pattern.kind() {
            SexpType::Integer => match rvalues::ints(pattern)[0] {
                i32::MIN => return Err(missing()),
                number => Some(f64::from(number)),
            },
            SexpType::Real => {
                let number = rvalues::reals(pattern)[0];
                if number.is_nan() {
                    return Err(missing());
                }
                Some(number)
            }
            SexpType::I64 => {
                // The interpreter converts a double to an `I64` exactly, so a pattern no double
                // holds exactly (past 2^53, or `i64::MAX`, which rounds to 2^63) matches none.
                let number = pattern.slice::<i64>()[0];
                let double = number as f64;
                (double < 9_223_372_036_854_775_808.0 && double as i64 == number).then_some(double)
            }
            _ => None,
        };
        let logical = match pattern.kind() {
            SexpType::Logical => match rvalues::ints(pattern)[0] {
                i32::MIN => return Err(missing()),
                flag => Some(i32::from(flag != 0)),
            },
            _ => None,
        };
        if pattern.kind() == SexpType::String && constant_string(pattern).is_none() {
            return Err(missing());
        }
        let condition = match self.builder.ty(value) {
            Type::I64 => {
                let converted = interp::numbers::read(pattern).or_else(|| {
                    interp::numbers::r_source(pattern)
                        .ok()
                        .and_then(interp::numbers::convert)
                });
                let Some(number) = converted else {
                    return Ok(self.never(fail));
                };
                let number = self
                    .builder
                    .emit(Type::I64, Op::Const(Constant::I64(number)));
                self.builder.emit(
                    Type::Bool,
                    Op::Compare(Compare::Eq, Item::I64, value, number),
                )
            }
            Type::Int => match number {
                Some(number)
                    if number.fract() == 0.0
                        && number > f64::from(i32::MIN)
                        && number <= f64::from(i32::MAX) =>
                {
                    let number = self.int(number as i32);
                    self.builder.emit(Type::Bool, Op::Same(value, number))
                }
                _ => return Ok(self.never(fail)),
            },
            Type::Dbl => {
                let Some(number) = number else {
                    return Ok(self.never(fail));
                };
                let number = self
                    .builder
                    .emit(Type::Dbl, Op::Const(Constant::Dbl(number)));
                // An `NA` or `NaN` compares as `NA`, which is not true.
                let equal = self.builder.emit(
                    Type::Lgl,
                    Op::Compare(Compare::Eq, Item::Dbl, value, number),
                );
                let true_ = self.builder.emit(Type::Lgl, Op::Const(Constant::Lgl(1)));
                self.builder.emit(Type::Bool, Op::Same(equal, true_))
            }
            Type::Lgl | Type::Bool => {
                let Some(flag) = logical else {
                    return Ok(self.never(fail));
                };
                let flag = self.builder.emit(Type::Lgl, Op::Const(Constant::Lgl(flag)));
                self.builder.emit(Type::Bool, Op::Same(value, flag))
            }
            Type::Str => {
                if pattern.kind() != SexpType::String {
                    return Ok(self.never(fail));
                }
                let text = self.value(pattern)?;
                self.builder
                    .emit(Type::Bool, Op::Compare(Compare::Eq, Item::Str, value, text))
            }
            Type::Record(_) | Type::Tag(_) => return Ok(self.never(fail)),
            _ => {
                return Err(Refusal::new(
                    "a literal pattern against a value that is not a scalar",
                ));
            }
        };
        self.pass_if(condition, fail);
        Ok(Test::Maybe)
    }

    /// A list pattern `[a, b, ..rest, z]`, which matches a ry list of exactly its items, or with a
    /// `..` at least the items before and after it, the items between binding to `..name`.
    unsafe fn test_list(
        &mut self,
        items: &[(Sexp, Sexp)],
        value: Local,
        fail: BlockId,
        bindings: &mut SmallVec<[(Sexp, Local); 4]>,
    ) -> Result<Test, Refusal> {
        let ty = self.builder.ty(value);
        let item = match ty {
            Type::Vector { item, array: true } => item,
            Type::Record(_)
            | Type::Tag(_)
            | Type::Int
            | Type::Dbl
            | Type::Lgl
            | Type::Bool
            | Type::Str
            | Type::I64 => return Ok(self.never(fail)),
            _ => {
                return Err(Refusal::new(
                    "a list pattern against a value that is not a ry list",
                ));
            }
        };
        let rests: SmallVec<[usize; 2]> = items
            .iter()
            .enumerate()
            .filter(|&(_, &(_, item))| rest_name(item).is_some())
            .map(|(position, _)| position)
            .collect();
        let (before, after, rest) = match rests[..] {
            [] => (items, &items[items.len()..], None),
            [position] => (
                &items[..position],
                &items[position + 1..],
                rest_name(items[position].1).flatten(),
            ),
            _ => return Err(Refusal::new("a list pattern with two `..`")),
        };
        let fixed = (before.len() + after.len()) as i64;
        let count = self.builder.emit(Type::I64, Op::Length(value));
        let fixed = self
            .builder
            .emit(Type::I64, Op::Const(Constant::I64(fixed)));
        let compare = if rests.is_empty() {
            Compare::Eq
        } else {
            Compare::Ge
        };
        let long_enough = self
            .builder
            .emit(Type::Bool, Op::Compare(compare, Item::I64, count, fixed));
        self.pass_if(long_enough, fail);
        if !before.is_empty() || !after.is_empty() || rest.is_some() {
            self.known_items(value)?;
        }
        for (offset, &(_, pattern)) in before.iter().enumerate() {
            let position = self
                .builder
                .emit(Type::I64, Op::Const(Constant::I64(offset as i64 + 1)));
            let item = self
                .builder
                .emit(Type::of_item(item), Op::Get(value, position));
            if self.test_pattern(pattern, item, Known::Typed, fail, bindings)? == Test::Never {
                return Ok(Test::Never);
            }
        }
        for (offset, &(_, pattern)) in after.iter().enumerate() {
            let from_end = (after.len() - offset - 1) as i64;
            let from_end = self
                .builder
                .emit(Type::I64, Op::Const(Constant::I64(from_end)));
            let position = self
                .builder
                .emit(Type::I64, Op::Arith(Arith::Sub, Item::I64, count, from_end));
            let item = self
                .builder
                .emit(Type::of_item(item), Op::Get(value, position));
            if self.test_pattern(pattern, item, Known::Typed, fail, bindings)? == Test::Never {
                return Ok(Test::Never);
            }
        }
        if let Some(name) = rest {
            let start = self
                .builder
                .emit(Type::I64, Op::Const(Constant::I64(before.len() as i64)));
            let between = self
                .builder
                .emit(Type::I64, Op::Arith(Arith::Sub, Item::I64, count, fixed));
            let slice = self.builder.emit(ty, Op::Slice(value, start, between));
            let symbol = rvalues::install(&name).map_err(|_| Refusal::new("a rest name"))?;
            bindings.push((symbol, slice));
        }
        Ok(Test::Maybe)
    }

    /// A tuple, record or tag pattern: a test of the value's shape unless `known` or the value's
    /// type settles it, then a test of each field.
    unsafe fn test_constructor(
        &mut self,
        pattern: Sexp,
        value: Local,
        known: Known,
        fail: BlockId,
        bindings: &mut SmallVec<[(Sexp, Local); 4]>,
    ) -> Result<Test, Refusal> {
        let (constructor, payload) = pattern_layout(pattern)?;
        let ty = self.builder.ty(value);
        if !matches!(ty, Type::Record(_) | Type::Tag(_)) {
            return Ok(self.never(fail));
        }
        let (shapes, tested) = match known {
            Known::Shape(shape) => {
                let fitting: SmallVec<[u32; 4]> = constructor_fits(&constructor, shape)
                    .map(|_| shape)
                    .into_iter()
                    .collect();
                (fitting, false)
            }
            Known::Other => return Ok(self.never(fail)),
            Known::Typed => {
                let (layouts, tags): (SmallVec<[u32; 1]>, SmallVec<[u32; 1]>) = match constructor {
                    PatternLayout::Exact(layout, _) => {
                        (smallvec::smallvec![layout], SmallVec::new())
                    }
                    PatternLayout::Tag(tag) | PatternLayout::Open { tag: Some(tag), .. } => {
                        (SmallVec::new(), smallvec::smallvec![tag])
                    }
                    // A record value has its one shape, and a tag never matches a record.
                    PatternLayout::Open { tag: None, .. } => (SmallVec::new(), SmallVec::new()),
                };
                let (candidates, exhaustive) = self.shapes_of(ty, &layouts, &tags)?;
                let fitting: SmallVec<[u32; 4]> = candidates
                    .iter()
                    .copied()
                    .filter(|&shape| constructor_fits(&constructor, shape).is_some())
                    .collect();
                let settled = exhaustive && fitting.len() == candidates.len();
                (fitting, !settled)
            }
        };
        if shapes.is_empty() {
            return Ok(self.never(fail));
        }
        let reads_fields = !matches!(constructor, PatternLayout::Tag(_))
            && payload.iter().any(|&(_, item)| !is_wildcard(item));
        if shapes.len() > 1 && reads_fields {
            return Err(Refusal::new(
                "a pattern inside another over shapes that hold different fields",
            ));
        }
        if tested {
            let shape_of = self.builder.emit(Type::Int, Op::ShapeOf(value));
            let pass = self.builder.block();
            for &shape in &shapes {
                let id = self.shape_id(shape);
                let same = self.builder.emit(Type::Bool, Op::Same(shape_of, id));
                let next = self.builder.block();
                self.builder.terminate(Terminator::Branch {
                    condition: same,
                    then: pass,
                    otherwise: next,
                });
                self.builder.switch_to(next);
            }
            self.builder.terminate(Terminator::Jump(fail));
            self.builder.switch_to(pass);
        }
        let mut test = if tested { Test::Maybe } else { Test::Always };
        if let ([shape], true) = (&shapes[..], reads_fields) {
            let slots = constructor_fits(&constructor, *shape)
                .ok_or(Refusal::new("a pattern that does not fit its shape"))?;
            for (&(_, item), slot) in payload.iter().zip(slots) {
                let field = self.field(value, *shape, slot)?;
                match self.test_pattern(item, field, Known::Typed, fail, bindings)? {
                    Test::Never => return Ok(Test::Never),
                    Test::Maybe => test = Test::Maybe,
                    Test::Always => {}
                }
            }
        }
        Ok(test)
    }

    /// Continues where `condition`, a `Bool`, holds, and jumps to `fail` where it does not.
    fn pass_if(&mut self, condition: Local, fail: BlockId) {
        let pass = self.builder.block();
        self.builder.terminate(Terminator::Branch {
            condition,
            then: pass,
            otherwise: fail,
        });
        self.builder.switch_to(pass);
    }

    /// A pattern that cannot match here: the code jumps to `fail`.
    fn never(&mut self, fail: BlockId) -> Test {
        self.builder.terminate(Terminator::Jump(fail));
        Test::Never
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
            Expected::Free,
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

    /// `Result.map`, `Result.map_err`, `Result.map_both`, `Result.and_then` and
    /// `Result.on_err`: a switch on the result's shape whose arm for each side inlines that
    /// side's callback, and passes a side without one through unchanged. `map` and `map_err` wrap
    /// the callback's value in the side's tag; `and_then` and `on_err` give it as it is, which
    /// must itself be a result, as the interpreter requires.
    unsafe fn result_map(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let both = name == "Result.map_both";
        let (result, on_ok, on_err) = match arguments.exactly(if both { 3 } else { 2 })?[..] {
            [result, on_ok, on_err] => (result, Some(on_ok), Some(on_err)),
            [result, f] if matches!(name, "Result.map" | "Result.and_then") => {
                (result, Some(f), None)
            }
            [result, f] => (result, None, Some(f)),
            _ => return Err(Refusal(Cow::Owned(format!("a malformed `{name}`")))),
        };
        let chained = matches!(name, "Result.and_then" | "Result.on_err");
        let result = self.value(result)?;
        let on_ok = on_ok.map(|f| self.function_argument(f)).transpose()?;
        let on_err = on_err.map(|f| self.function_argument(f)).transpose()?;
        let (ok, err) = (tag_layout("Ok", 1)?, tag_layout("Err", 1)?);
        let (candidates, exhaustive) = self.shapes_of(self.builder.ty(result), &[ok, err], &[])?;
        let value = self.switch_shapes(
            result,
            &candidates,
            exhaustive,
            Expected::Free,
            &mut |lowering, shape| {
                let (shape, callback, label) = match shape {
                    Some(shape) if shapes::shape(shape).layout == ok => (shape, on_ok, "Ok"),
                    Some(shape) if shapes::shape(shape).layout == err => (shape, on_err, "Err"),
                    _ => {
                        lowering.builder.terminate(Terminator::Fail);
                        let unreachable = lowering.builder.block();
                        lowering.builder.switch_to(unreachable);
                        return Ok(None);
                    }
                };
                let Some(callback) = callback else {
                    // The side passes through as this shape alone, so the result's type gains
                    // none of the operand's shapes for the other side.
                    let ty = Type::Tag(shapes::union_of(&[shape]));
                    return Ok(Some(lowering.builder.emit(ty, Op::Relabel(result, ty))));
                };
                let payload = lowering.field(result, shape, 0)?;
                let value = lowering
                    .apply(callback, &[payload])?
                    .ok_or(Refusal::new("a result callback that never completes"))?;
                if !chained {
                    return lowering.tag(label, &[value]).map(Some);
                }
                let is_result = match lowering.builder.ty(value) {
                    Type::Tag(union) => shapes::members(union).is_some_and(|members| {
                        members.iter().all(|&member| {
                            let layout = shapes::shape(member).layout;
                            layout == ok || layout == err
                        })
                    }),
                    _ => false,
                };
                if !is_result {
                    return Err(Refusal(Cow::Owned(format!(
                        "a `{name}` callback that may give something other than a result"
                    ))));
                }
                Ok(Some(value))
            },
        )?;
        let value = value.ok_or(Refusal(Cow::Owned(format!("a `{name}` of no result"))))?;
        self.usable(value)?;
        Ok(value)
    }

    /// `Result.map2(first, second, f)`: `:Ok(f(a, b))` of two `:Ok`s, else the first `:Err`, in
    /// a switch on each result's shape with `f` inlined where both are `:Ok`.
    unsafe fn result_map2(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [first, second, function] = arguments.exactly(3)?[..] else {
            return Err(Refusal::new("a malformed `Result.map2`"));
        };
        let first = self.value(first)?;
        let second = self.value(second)?;
        let callable = self.function_argument(function)?;
        let (ok, err) = (tag_layout("Ok", 1)?, tag_layout("Err", 1)?);
        let (first_shapes, first_exhaustive) =
            self.shapes_of(self.builder.ty(first), &[ok, err], &[])?;
        let (second_shapes, second_exhaustive) =
            self.shapes_of(self.builder.ty(second), &[ok, err], &[])?;
        let value = self.switch_shapes(
            first,
            &first_shapes,
            first_exhaustive,
            Expected::Free,
            &mut |lowering, shape| {
                let Some(left_shape) = shape else {
                    return Ok(lowering.unreachable());
                };
                if shapes::shape(left_shape).layout == err {
                    let ty = Type::Tag(shapes::union_of(&[left_shape]));
                    return Ok(Some(lowering.builder.emit(ty, Op::Relabel(first, ty))));
                }
                if shapes::shape(left_shape).layout != ok {
                    return Ok(lowering.unreachable());
                }
                let left = lowering.field(first, left_shape, 0)?;
                lowering.switch_shapes(
                    second,
                    &second_shapes,
                    second_exhaustive,
                    Expected::Free,
                    &mut |lowering, shape| {
                        let Some(right_shape) = shape else {
                            return Ok(lowering.unreachable());
                        };
                        let layout = shapes::shape(right_shape).layout;
                        if layout == err {
                            let ty = Type::Tag(shapes::union_of(&[right_shape]));
                            return Ok(Some(lowering.builder.emit(ty, Op::Relabel(second, ty))));
                        }
                        if layout != ok {
                            return Ok(lowering.unreachable());
                        }
                        let right = lowering.field(second, right_shape, 0)?;
                        let value =
                            lowering
                                .apply(callable, &[left, right])?
                                .ok_or(Refusal::new(
                                    "a `Result.map2` callback that never completes",
                                ))?;
                        lowering.tag("Ok", &[value]).map(Some)
                    },
                )
            },
        )?;
        let value = value.ok_or(Refusal::new("a `Result.map2` of no result"))?;
        self.usable(value)?;
        Ok(value)
    }

    /// Ends the current block as one no value reaches, and continues in a fresh block after it.
    fn unreachable(&mut self) -> Option<Local> {
        self.builder.terminate(Terminator::Fail);
        let unreachable = self.builder.block();
        self.builder.switch_to(unreachable);
        None
    }

    /// `Result.is_ok(result)` or `Result.is_err(result)`.
    unsafe fn result_is(
        &mut self,
        ok_wanted: bool,
        arguments: &Arguments,
    ) -> Result<Local, Refusal> {
        let [result] = arguments.exactly(1)?[..] else {
            return Err(Refusal::new("a malformed `Result.is_ok`"));
        };
        let result = self.value(result)?;
        let (ok, err) = (tag_layout("Ok", 1)?, tag_layout("Err", 1)?);
        let wanted = if ok_wanted { ok } else { err };
        let (candidates, exhaustive) = self.shapes_of(self.builder.ty(result), &[ok, err], &[])?;
        let answer = self.switch_shapes(
            result,
            &candidates,
            exhaustive,
            Expected::Free,
            &mut |lowering, shape| match shape.map(|shape| shapes::shape(shape).layout) {
                Some(layout) if layout == ok || layout == err => {
                    let truth = Constant::Lgl(i32::from(layout == wanted));
                    Ok(Some(lowering.builder.emit(Type::Lgl, Op::Const(truth))))
                }
                _ => {
                    lowering.builder.terminate(Terminator::Fail);
                    let unreachable = lowering.builder.block();
                    lowering.builder.switch_to(unreachable);
                    Ok(None)
                }
            },
        )?;
        let answer = answer.ok_or(Refusal::new("a `Result.is_ok` of no result"))?;
        Ok(self.builder.emit(Type::Bool, Op::Truth(answer)))
    }

    /// `try result`: the payload of an `:Ok`, or a return of the `:Err` itself from the function.
    /// Any other value gives up, and the interpreter raises the error.
    unsafe fn try_result(&mut self, call: Sexp) -> Result<Option<Local>, Refusal> {
        let [result] = self.arguments(call, 1)?[..] else {
            return Err(Refusal::new("a malformed `try`"));
        };
        let result = self.value(result)?;
        let (ok, err) = (tag_layout("Ok", 1)?, tag_layout("Err", 1)?);
        let (candidates, exhaustive) = self.shapes_of(self.builder.ty(result), &[ok, err], &[])?;
        self.switch_shapes(
            result,
            &candidates,
            exhaustive,
            Expected::Free,
            &mut |lowering, shape| match shape {
                Some(shape) if shapes::shape(shape).layout == ok => {
                    lowering.field(result, shape, 0).map(Some)
                }
                Some(shape) if shapes::shape(shape).layout == err => {
                    // The returned type is this `:Err` shape alone, so the function's result type
                    // gains no `:Ok` shape of the operand's.
                    let ty = Type::Tag(shapes::union_of(&[shape]));
                    let error = lowering.builder.emit(ty, Op::Relabel(result, ty));
                    lowering.exit(error)?;
                    Ok(None)
                }
                _ => {
                    lowering.builder.terminate(Terminator::Fail);
                    let unreachable = lowering.builder.block();
                    lowering.builder.switch_to(unreachable);
                    Ok(None)
                }
            },
        )
    }

    /// `List.first(list)`, `List.last(list)` or `List.get(list, index)`: `:Ok` of the item at
    /// a 1-based position, or an `:Err` when the list has none there.
    unsafe fn list_get(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let (list, index) = match arguments.exactly(if name == "List.get" { 2 } else { 1 })?[..] {
            [list] => (list, None),
            [list, index] => (list, Some(index)),
            _ => return Err(Refusal(Cow::Owned(format!("a malformed `{name}`")))),
        };
        let (list, ty) = self.known_list(list, name)?;
        let item = ty.items().ok_or(Refusal::new(
            "a `List.get` of something other than a ry list",
        ))?;
        let count = self.builder.emit(Type::I64, Op::Length(list));
        let one = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
        let (found, missing) = (self.builder.block(), self.builder.block());
        let position = match index {
            Some(index) => {
                let index = self.value(index)?;
                let double = self.builder.ty(index) == Type::Dbl;
                let index = self.to_i64(index)?;
                if double {
                    // The library reads a double index only up to the largest R integer.
                    let size = self
                        .builder
                        .emit(Type::I64, Op::Math(Math::Abs, Item::I64, index));
                    let largest = self
                        .builder
                        .emit(Type::I64, Op::Const(Constant::I64(i64::from(i32::MAX))));
                    let outside = self.builder.emit(
                        Type::Lgl,
                        Op::Compare(Compare::Gt, Item::I64, size, largest),
                    );
                    self.fail_if(outside);
                }
                let below = self.test(Compare::Lt, Item::I64, index, one);
                let inside = self.builder.block();
                self.builder.terminate(Terminator::Branch {
                    condition: below,
                    then: missing,
                    otherwise: inside,
                });
                self.builder.switch_to(inside);
                let above = self.test(Compare::Gt, Item::I64, index, count);
                self.builder.terminate(Terminator::Branch {
                    condition: above,
                    then: missing,
                    otherwise: found,
                });
                index
            }
            None => {
                let empty = self.test(Compare::Lt, Item::I64, count, one);
                self.builder.terminate(Terminator::Branch {
                    condition: empty,
                    then: missing,
                    otherwise: found,
                });
                if name == "List.first" { one } else { count }
            }
        };
        self.builder.switch_to(found);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(list, position));
        let ok = self.tag("Ok", &[element])?;
        let found = self.builder.current;
        self.builder.switch_to(missing);
        let reason = self.tag(
            if name == "List.get" {
                "OutOfBounds"
            } else {
                "ListWasEmpty"
            },
            &[],
        )?;
        let err = self.tag("Err", &[reason])?;
        self.joined_result(found, ok, err)
    }

    /// A test of two scalars as a `Bool`.
    fn test(&mut self, op: Compare, item: Item, a: Local, b: Local) -> Local {
        let test = self.builder.emit(Type::Lgl, Op::Compare(op, item, a, b));
        self.builder.emit(Type::Bool, Op::Truth(test))
    }

    /// A number argument of a library function as the library reads it: an `I64`, an R integer
    /// or a double. The call fails on `NA` and `NaN`, and the interpreter answers for them.
    unsafe fn library_number(&mut self, value: Local) -> Result<Local, Refusal> {
        match self.builder.ty(value) {
            Type::I64 => {}
            // `ToI64` fails on `NA`.
            Type::Int => {
                self.builder.emit(Type::I64, Op::ToI64(value));
            }
            Type::Dbl => self.fail_if_nan(value),
            _ => {
                return Err(Refusal::new(
                    "a library function of something other than a number",
                ));
            }
        }
        Ok(value)
    }

    /// A `Num` function of one number, as the library computes it: a double result that is
    /// `NaN` and a logarithm of a number that is not positive are errors the interpreter raises.
    unsafe fn number_function(
        &mut self,
        name: &str,
        arguments: &Arguments,
    ) -> Result<Local, Refusal> {
        let [number] = arguments.exactly(1)?[..] else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        let number = self.value(number)?;
        let number = self.library_number(number)?;
        let integer = self.builder.ty(number) == Type::I64;
        if !integer && matches!(name, "Num.is_even" | "Num.is_odd") {
            return Err(Refusal(Cow::Owned(format!("a `{name}` of an R number"))));
        }
        match name {
            "Num.is_zero" | "Num.is_positive" | "Num.is_negative" => {
                let op = match name {
                    "Num.is_zero" => Compare::Eq,
                    "Num.is_positive" => Compare::Gt,
                    _ => Compare::Lt,
                };
                if integer {
                    let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
                    return Ok(self.test(op, Item::I64, number, zero));
                }
                let number = self.as_double(number)?;
                let zero = self.builder.emit(Type::Dbl, Op::Const(Constant::Dbl(0.0)));
                Ok(self.test(op, Item::Dbl, number, zero))
            }
            "Num.is_even" | "Num.is_odd" => {
                let two = self.builder.emit(Type::I64, Op::Const(Constant::I64(2)));
                let remainder = self.builder.emit(
                    Type::I64,
                    Op::Arith(Arith::RemTrunc, Item::I64, number, two),
                );
                let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
                let op = if name == "Num.is_even" {
                    Compare::Eq
                } else {
                    Compare::Ne
                };
                Ok(self.test(op, Item::I64, remainder, zero))
            }
            "Num.neg" => match self.builder.ty(number) {
                Type::I64 => Ok(self
                    .builder
                    .emit(Type::I64, Op::Math(Math::Neg, Item::I64, number))),
                Type::Int => Ok(self
                    .builder
                    .emit(Type::Int, Op::Math(Math::Neg, Item::Int, number))),
                _ => Ok(self
                    .builder
                    .emit(Type::Dbl, Op::Math(Math::Neg, Item::Dbl, number))),
            },
            "Num.to_i64" => self.to_i64(number),
            _ if integer => Err(Refusal(Cow::Owned(format!(
                "a `{name}` of an I64, which is an error"
            )))),
            "Num.is_finite" | "Num.is_infinite" => {
                let number = self.as_double(number)?;
                let size = self
                    .builder
                    .emit(Type::Dbl, Op::Math(Math::Abs, Item::Dbl, number));
                let infinity = self
                    .builder
                    .emit(Type::Dbl, Op::Const(Constant::Dbl(f64::INFINITY)));
                let op = if name == "Num.is_finite" {
                    Compare::Lt
                } else {
                    Compare::Eq
                };
                Ok(self.test(op, Item::Dbl, size, infinity))
            }
            "Num.floor" | "Num.ceiling" => {
                if self.builder.ty(number) == Type::Int {
                    return Ok(self.builder.emit(Type::I64, Op::ToI64(number)));
                }
                let op = if name == "Num.floor" {
                    Math::Floor
                } else {
                    Math::Ceiling
                };
                let whole = self
                    .builder
                    .emit(Type::Dbl, Op::Math(op, Item::Dbl, number));
                Ok(self.builder.emit(Type::I64, Op::ToI64(whole)))
            }
            _ => {
                let number = self.as_double(number)?;
                let op = match name {
                    "Num.log" => {
                        let zero = self.builder.emit(Type::Dbl, Op::Const(Constant::Dbl(0.0)));
                        let not_positive = self
                            .builder
                            .emit(Type::Lgl, Op::Compare(Compare::Le, Item::Dbl, number, zero));
                        self.fail_if(not_positive);
                        Math::Log
                    }
                    "Num.sin" => Math::Sin,
                    "Num.cos" => Math::Cos,
                    "Num.tan" => Math::Tan,
                    "Num.atan" => Math::Atan,
                    "Num.asin" => Math::Asin,
                    _ => Math::Acos,
                };
                // The math function gives up on a `NaN` result, which the library refuses.
                Ok(self
                    .builder
                    .emit(Type::Dbl, Op::Math(op, Item::Dbl, number)))
            }
        }
    }

    /// `Num.add_wrap` and its siblings, the checked and saturating forms, and `Num.div_ceil`,
    /// on `I64`s. On R numbers the library works on 32 bits, which the typed core does not.
    unsafe fn overflowing(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let [a, b] = arguments.exactly(2)?[..] else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        let a = self.value(a)?;
        let a = self.library_number(a)?;
        let b = self.value(b)?;
        let b = self.library_number(b)?;
        if self.builder.ty(a) != Type::I64 && self.builder.ty(b) != Type::I64 {
            return Err(Refusal(Cow::Owned(format!("a `{name}` of R numbers"))));
        }
        let a = self.to_i64(a)?;
        let b = self.to_i64(b)?;
        if name == "Num.div_ceil" {
            return Ok(self.ceiling_division(a, b));
        }
        let (operation, behavior) = name
            .strip_prefix("Num.")
            .and_then(|name| name.split_once('_'))
            .ok_or(Refusal::new("a malformed overflowing operation"))?;
        let op = match operation {
            "add" => Arith::Add,
            "sub" => Arith::Sub,
            _ => Arith::Mul,
        };
        let overflow = match behavior {
            "wrap" => Overflow::Wrap,
            "saturated" => Overflow::Saturate,
            _ => {
                let overflowed = self
                    .builder
                    .emit(Type::Bool, Op::Overflowing(Overflow::Test, op, a, b));
                let (fits, overflows) = (self.builder.block(), self.builder.block());
                self.builder.terminate(Terminator::Branch {
                    condition: overflowed,
                    then: overflows,
                    otherwise: fits,
                });
                self.builder.switch_to(fits);
                let value = self
                    .builder
                    .emit(Type::I64, Op::Overflowing(Overflow::Wrap, op, a, b));
                let ok = self.tag("Ok", &[value])?;
                self.builder.switch_to(overflows);
                let reason = self.tag("Overflow", &[])?;
                let err = self.tag("Err", &[reason])?;
                return self.joined_result(fits, ok, err);
            }
        };
        Ok(self
            .builder
            .emit(Type::I64, Op::Overflowing(overflow, op, a, b)))
    }

    /// `Num.to_i64_checked(number)`: `:Ok` of an `I64`, or of an `F64` that is a whole number
    /// an `I64` holds, and `:Err(:OutOfBounds)` for any other `F64`.
    unsafe fn to_i64_checked(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [number] = arguments.exactly(1)?[..] else {
            return Err(Refusal::new("a malformed `Num.to_i64_checked`"));
        };
        let number = self.value(number)?;
        match self.builder.ty(number) {
            Type::I64 => return self.tag("Ok", &[number]),
            Type::Dbl => {}
            _ => {
                return Err(Refusal::new(
                    "a `Num.to_i64_checked` of something other than an I64 or an F64",
                ));
            }
        }
        // A whole number from -2^63 up to but not including 2^63. `NaN` skips the tests, whose
        // comparisons would give `NA`.
        let (exact, inexact, checking) = (
            self.builder.block(),
            self.builder.block(),
            self.builder.block(),
        );
        let missing = self.builder.emit(Type::Bool, Op::IsNan(number));
        self.builder.terminate(Terminator::Branch {
            condition: missing,
            then: inexact,
            otherwise: checking,
        });
        self.builder.switch_to(checking);
        let whole = self
            .builder
            .emit(Type::Dbl, Op::Math(Math::Floor, Item::Dbl, number));
        let compare = |lowering: &mut Self, op: Compare, a: Local, b: Local| {
            lowering
                .builder
                .emit(Type::Lgl, Op::Compare(op, Item::Dbl, a, b))
        };
        let is_whole = compare(self, Compare::Eq, whole, number);
        let low = self.builder.emit(
            Type::Dbl,
            Op::Const(Constant::Dbl(-9_223_372_036_854_775_808.0)),
        );
        let high = self.builder.emit(
            Type::Dbl,
            Op::Const(Constant::Dbl(9_223_372_036_854_775_808.0)),
        );
        let above = compare(self, Compare::Ge, number, low);
        let below = compare(self, Compare::Lt, number, high);
        let within = self.builder.emit(Type::Lgl, Op::And(above, below));
        let fits = self.builder.emit(Type::Lgl, Op::And(is_whole, within));
        let fits = self.builder.emit(Type::Bool, Op::Truth(fits));
        self.builder.terminate(Terminator::Branch {
            condition: fits,
            then: exact,
            otherwise: inexact,
        });
        self.builder.switch_to(exact);
        let value = self.builder.emit(Type::I64, Op::ToI64(number));
        let ok = self.tag("Ok", &[value])?;
        self.builder.switch_to(inexact);
        let reason = self.tag("OutOfBounds", &[])?;
        let err = self.tag("Err", &[reason])?;
        self.joined_result(exact, ok, err)
    }

    /// `Num.div_checked`, `Num.sqrt_checked` and `Num.log_checked` on doubles, and
    /// `Num.div_trunc_checked`, `Num.div_ceil_checked` and `Num.rem_checked` on `I64`s: `:Ok` of
    /// the operation, or the `:Err` the library gives for the operands it rejects.
    unsafe fn checked_number(
        &mut self,
        name: &str,
        arguments: &Arguments,
    ) -> Result<Local, Refusal> {
        let unary = matches!(name, "Num.sqrt_checked" | "Num.log_checked");
        let parameters = arguments.exactly(if unary { 1 } else { 2 })?;
        let mut numbers: SmallVec<[Local; 2]> = SmallVec::new();
        for &parameter in &parameters {
            let value = self.value(parameter)?;
            numbers.push(self.library_number(value)?);
        }
        let integral = matches!(
            name,
            "Num.div_trunc_checked" | "Num.div_ceil_checked" | "Num.rem_checked"
        );
        // The interpreter reads `I64`s apart from R's numbers, so only these operand types give
        // the result it gives.
        let operand = if integral { Type::I64 } else { Type::Dbl };
        if numbers
            .iter()
            .any(|&number| self.builder.ty(number) != operand)
        {
            return Err(Refusal(Cow::Owned(format!(
                "a `{name}` of numbers it does not compute natively"
            ))));
        }
        let (rejected, reason) = if integral {
            let b = numbers[1];
            let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
            (self.test(Compare::Eq, Item::I64, b, zero), "DivByZero")
        } else {
            let zero = self.builder.emit(Type::Dbl, Op::Const(Constant::Dbl(0.0)));
            match name {
                "Num.div_checked" => (
                    self.test(Compare::Eq, Item::Dbl, numbers[1], zero),
                    "DivByZero",
                ),
                "Num.sqrt_checked" => (
                    self.test(Compare::Lt, Item::Dbl, numbers[0], zero),
                    "SqrtOfNegative",
                ),
                _ => (
                    self.test(Compare::Le, Item::Dbl, numbers[0], zero),
                    "LogNeedsPositive",
                ),
            }
        };
        let (computed, refused) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: rejected,
            then: refused,
            otherwise: computed,
        });
        self.builder.switch_to(computed);
        let value = match name {
            "Num.div_ceil_checked" => self.ceiling_division(numbers[0], numbers[1]),
            "Num.div_trunc_checked" | "Num.rem_checked" => {
                let op = if name == "Num.rem_checked" {
                    Arith::RemTrunc
                } else {
                    Arith::DivTrunc
                };
                self.builder
                    .emit(Type::I64, Op::Arith(op, Item::I64, numbers[0], numbers[1]))
            }
            "Num.sqrt_checked" => self
                .builder
                .emit(Type::Dbl, Op::Math(Math::Sqrt, Item::Dbl, numbers[0])),
            "Num.log_checked" => self
                .builder
                .emit(Type::Dbl, Op::Math(Math::Log, Item::Dbl, numbers[0])),
            _ => {
                // A quotient that is `NaN` (`Inf / Inf`) is `:Err(:Undefined)`, which the
                // interpreter gives.
                let quotient = self.builder.emit(
                    Type::Dbl,
                    Op::Arith(Arith::Div, Item::Dbl, numbers[0], numbers[1]),
                );
                self.fail_if_nan(quotient);
                quotient
            }
        };
        let ok = self.tag("Ok", &[value])?;
        let computed_end = self.builder.current;
        self.builder.switch_to(refused);
        let reason = self.tag(reason, &[])?;
        let err = self.tag("Err", &[reason])?;
        self.joined_result(computed_end, ok, err)
    }

    /// `Num.is_approx_eq(first, second, rtol, atol)`: whether the numbers are equal or differ by
    /// at most the larger of `atol` and `rtol` times the larger magnitude.
    unsafe fn is_approx_eq(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        // Named arguments take their formals and the others fill the rest in order, as R
        // matches them.
        let formals = ["first", "second", "rtol", "atol"];
        let mut matched: [Option<Sexp>; 4] = [None; 4];
        for (tag, value) in &arguments.items {
            let Some(tag) = tag else { continue };
            let slot = formals
                .iter()
                .position(|formal| formal == tag)
                .and_then(|at| matched.get_mut(at))
                .filter(|slot| slot.is_none())
                .ok_or(Refusal::new(
                    "a `Num.is_approx_eq` with unexpected arguments",
                ))?;
            *slot = Some(*value);
        }
        let mut open = matched.iter_mut().filter(|slot| slot.is_none());
        for (tag, value) in &arguments.items {
            if tag.is_none() {
                let slot = open.next().ok_or(Refusal::new(
                    "a `Num.is_approx_eq` with unexpected arguments",
                ))?;
                *slot = Some(*value);
            }
        }
        let defaults = [None, None, Some(0.00001), Some(0.00000001)];
        let mut numbers: SmallVec<[Local; 4]> = SmallVec::new();
        for (argument, default) in matched.into_iter().zip(defaults) {
            let number = match (argument, default) {
                (Some(argument), _) => {
                    let value = self.value(argument)?;
                    let value = self.library_number(value)?;
                    self.as_double(value)?
                }
                (None, Some(default)) => self
                    .builder
                    .emit(Type::Dbl, Op::Const(Constant::Dbl(default))),
                (None, None) => {
                    return Err(Refusal::new("a `Num.is_approx_eq` without both numbers"));
                }
            };
            numbers.push(number);
        }
        let [first, second, rtol, atol] = numbers[..] else {
            return Err(Refusal::new("a malformed `Num.is_approx_eq`"));
        };
        let arith = |lowering: &mut Self, op: Arith, a: Local, b: Local| {
            lowering
                .builder
                .emit(Type::Dbl, Op::Arith(op, Item::Dbl, a, b))
        };
        let magnitude = |lowering: &mut Self, a: Local| {
            lowering
                .builder
                .emit(Type::Dbl, Op::Math(Math::Abs, Item::Dbl, a))
        };
        let first_magnitude = magnitude(self, first);
        let second_magnitude = magnitude(self, second);
        let larger = arith(self, Arith::Max, first_magnitude, second_magnitude);
        let relative = arith(self, Arith::Mul, rtol, larger);
        let tolerance = arith(self, Arith::Max, atol, relative);
        let difference = arith(self, Arith::Sub, first, second);
        let difference = magnitude(self, difference);
        let equal = self.builder.emit(
            Type::Lgl,
            Op::Compare(Compare::Eq, Item::Dbl, first, second),
        );
        let close = self.builder.emit(
            Type::Lgl,
            Op::Compare(Compare::Le, Item::Dbl, difference, tolerance),
        );
        let either = self.builder.emit(Type::Lgl, Op::Or(equal, close));
        Ok(self.builder.emit(Type::Bool, Op::Truth(either)))
    }

    /// `Num.div_ceil(a, b)` on `I64`s: the truncated quotient, one more when a remainder is left
    /// and the operands have one sign. The quotient gives up on a zero divisor, which the library
    /// raises.
    fn ceiling_division(&mut self, a: Local, b: Local) -> Local {
        let quotient = self
            .builder
            .emit(Type::I64, Op::Arith(Arith::DivTrunc, Item::I64, a, b));
        let remainder = self
            .builder
            .emit(Type::I64, Op::Arith(Arith::RemTrunc, Item::I64, a, b));
        let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
        let signs = self.builder.emit(Type::I64, Op::Bits(Bits::Xor, a, b));
        let left = self.builder.emit(
            Type::Lgl,
            Op::Compare(Compare::Ne, Item::I64, remainder, zero),
        );
        let alike = self
            .builder
            .emit(Type::Lgl, Op::Compare(Compare::Ge, Item::I64, signs, zero));
        let both = self.builder.emit(Type::Lgl, Op::And(left, alike));
        let rounds_up = self.builder.emit(Type::Bool, Op::Truth(both));
        let result = self.builder.local(Type::I64);
        self.builder.push(Some(result), Op::Copy(quotient));
        let (up, join) = (self.builder.block(), self.builder.block());
        self.builder.terminate(Terminator::Branch {
            condition: rounds_up,
            then: up,
            otherwise: join,
        });
        self.builder.switch_to(up);
        let one = self.builder.emit(Type::I64, Op::Const(Constant::I64(1)));
        let next = self
            .builder
            .emit(Type::I64, Op::Arith(Arith::Add, Item::I64, quotient, one));
        self.builder.push(Some(result), Op::Copy(next));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(join);
        result
    }

    /// A `Num` function of two numbers, as the library computes it. With an `I64` the other
    /// number converts exactly and the operation is on `I64`s, which an overflow or a zero
    /// divisor fails; the arithmetic and `abs_diff` of two R integers give an R integer, and
    /// otherwise both are doubles, whose `NaN` results are errors.
    unsafe fn number_operation(
        &mut self,
        name: &str,
        arguments: &Arguments,
    ) -> Result<Local, Refusal> {
        let [a, b] = arguments.exactly(2)?[..] else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        let a = self.value(a)?;
        let a = self.library_number(a)?;
        let b = self.value(b)?;
        let b = self.library_number(b)?;
        let comparison = match name {
            "Num.is_lt" => Some(Compare::Lt),
            "Num.is_gt" => Some(Compare::Gt),
            "Num.is_lte" => Some(Compare::Le),
            "Num.is_gte" => Some(Compare::Ge),
            _ => None,
        };
        let arithmetic = match name {
            "Num.add" => Some(Arith::Add),
            "Num.sub" => Some(Arith::Sub),
            "Num.mul" => Some(Arith::Mul),
            _ => None,
        };
        let (first_type, second_type) = (self.builder.ty(a), self.builder.ty(b));
        if first_type == Type::I64 || second_type == Type::I64 {
            let a = self.to_i64(a)?;
            let b = self.to_i64(b)?;
            if let Some(op) = comparison {
                return Ok(self.test(op, Item::I64, a, b));
            }
            if let Some(op) = arithmetic {
                return Ok(self.builder.emit(Type::I64, Op::Arith(op, Item::I64, a, b)));
            }
            let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
            return match name {
                "Num.div_trunc" | "Num.rem" => {
                    let op = if name == "Num.rem" {
                        Arith::RemTrunc
                    } else {
                        Arith::DivTrunc
                    };
                    Ok(self.builder.emit(Type::I64, Op::Arith(op, Item::I64, a, b)))
                }
                "Num.abs_diff" => {
                    let high = self
                        .builder
                        .emit(Type::I64, Op::Arith(Arith::Max, Item::I64, a, b));
                    let low = self
                        .builder
                        .emit(Type::I64, Op::Arith(Arith::Min, Item::I64, a, b));
                    Ok(self
                        .builder
                        .emit(Type::I64, Op::Arith(Arith::Sub, Item::I64, high, low)))
                }
                "Num.is_multiple_of" => {
                    // Only zero is a multiple of zero, and a remainder by zero is not defined.
                    let result = self.builder.local(Type::Bool);
                    let by_zero = self.test(Compare::Eq, Item::I64, b, zero);
                    let (zero_divisor, divisor, join) = (
                        self.builder.block(),
                        self.builder.block(),
                        self.builder.block(),
                    );
                    self.builder.terminate(Terminator::Branch {
                        condition: by_zero,
                        then: zero_divisor,
                        otherwise: divisor,
                    });
                    self.builder.switch_to(zero_divisor);
                    let zero_dividend = self.test(Compare::Eq, Item::I64, a, zero);
                    self.builder.push(Some(result), Op::Copy(zero_dividend));
                    self.builder.terminate(Terminator::Jump(join));
                    self.builder.switch_to(divisor);
                    let remainder = self
                        .builder
                        .emit(Type::I64, Op::Arith(Arith::RemTrunc, Item::I64, a, b));
                    let whole = self.test(Compare::Eq, Item::I64, remainder, zero);
                    self.builder.push(Some(result), Op::Copy(whole));
                    self.builder.terminate(Terminator::Jump(join));
                    self.builder.switch_to(join);
                    Ok(result)
                }
                _ => Err(Refusal(Cow::Owned(format!(
                    "a `{name}` of an I64, which is an error"
                )))),
            };
        }
        if first_type == Type::Int && second_type == Type::Int {
            // R integer arithmetic gives up on a result outside the integers, which the
            // library raises as an overflow.
            if let Some(op) = arithmetic {
                return Ok(self.builder.emit(Type::Int, Op::Arith(op, Item::Int, a, b)));
            }
            if name == "Num.abs_diff" {
                let difference = self
                    .builder
                    .emit(Type::Int, Op::Arith(Arith::Sub, Item::Int, a, b));
                return Ok(self
                    .builder
                    .emit(Type::Int, Op::Math(Math::Abs, Item::Int, difference)));
            }
        }
        let a = self.as_double(a)?;
        let b = self.as_double(b)?;
        if let Some(op) = comparison {
            return Ok(self.test(op, Item::Dbl, a, b));
        }
        let value = match name {
            "Num.div" => {
                let zero = self.builder.emit(Type::Dbl, Op::Const(Constant::Dbl(0.0)));
                let by_zero = self
                    .builder
                    .emit(Type::Lgl, Op::Compare(Compare::Eq, Item::Dbl, b, zero));
                self.fail_if(by_zero);
                self.builder
                    .emit(Type::Dbl, Op::Arith(Arith::Div, Item::Dbl, a, b))
            }
            "Num.abs_diff" => {
                let difference = self
                    .builder
                    .emit(Type::Dbl, Op::Arith(Arith::Sub, Item::Dbl, a, b));
                self.builder
                    .emit(Type::Dbl, Op::Math(Math::Abs, Item::Dbl, difference))
            }
            _ => match arithmetic {
                Some(op) => self.builder.emit(Type::Dbl, Op::Arith(op, Item::Dbl, a, b)),
                None => {
                    return Err(Refusal(Cow::Owned(format!("a `{name}` of R numbers"))));
                }
            },
        };
        self.fail_if_nan(value);
        Ok(value)
    }

    /// A `Bool` function. The library refuses `NA`, so a logical that is `NA` fails the call.
    unsafe fn boolean(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        if name == "Bool.not" {
            let [value] = arguments.exactly(1)?[..] else {
                return Err(Refusal::new("a malformed `Bool.not`"));
            };
            let value = self.bool_argument(value)?;
            let flipped = self.builder.emit(Type::Lgl, Op::Not(value));
            return Ok(self.builder.emit(Type::Bool, Op::Truth(flipped)));
        }
        let [a, b] = arguments.exactly(2)?[..] else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        if matches!(name, "Bool.and" | "Bool.or") {
            // Both arguments are evaluated, as in a call of the library function.
            let a = self.bool_argument(a)?;
            let b = self.bool_argument(b)?;
            let op = if name == "Bool.and" {
                Op::And(a, b)
            } else {
                Op::Or(a, b)
            };
            let value = self.builder.emit(Type::Lgl, op);
            return Ok(self.builder.emit(Type::Bool, Op::Truth(value)));
        }
        let op = if name == "Bool.is_eq" {
            Compare::Eq
        } else {
            Compare::Ne
        };
        // `identical()` decides, which tells `NaN` from `NA` and both from numbers; the call
        // fails on them, and the interpreter answers.
        let a = self.value(a)?;
        let b = self.value(b)?;
        let (first_type, second_type) = (self.builder.ty(a), self.builder.ty(b));
        let flag = |ty: Type| matches!(ty, Type::Lgl | Type::Bool);
        if first_type == Type::I64 || second_type == Type::I64 {
            // The interpreter reads a number literal beside an `I64` as an `I64`, so the
            // literal converts; any other R number is never `identical()` to an `I64`.
            let literal = |lowerer: &Self, value: Local| {
                lowerer.builder.ty(value) == Type::I64 || lowerer.constant_double(value).is_some()
            };
            if !literal(self, a) || !literal(self, b) {
                return Err(Refusal::new("a `Bool.is_eq` of an I64 and an R number"));
            }
            let a = self.to_i64(a)?;
            let b = self.to_i64(b)?;
            return Ok(self.test(op, Item::I64, a, b));
        }
        if flag(first_type) && flag(second_type) {
            let a = self.truth(a);
            let b = self.truth(b);
            return Ok(self.test(op, Item::Lgl, a, b));
        }
        let item = match (first_type, second_type) {
            (Type::Str, Type::Str) => Item::Str,
            (Type::Int, Type::Int) | (Type::Dbl, Type::Dbl) => {
                // The call fails on `NA` and `NaN`, which `identical()` tells from numbers.
                self.library_number(a)?;
                self.library_number(b)?;
                if first_type == Type::Int {
                    Item::Int
                } else {
                    Item::Dbl
                }
            }
            _ => {
                return Err(Refusal::new(
                    "a `Bool.is_eq` of values other than two numbers, strings or Bools",
                ));
            }
        };
        Ok(self.test(op, item, a, b))
    }

    /// A `Bool` argument, failing the call on `NA`.
    unsafe fn bool_argument(&mut self, value: Sexp) -> Result<Local, Refusal> {
        let value = self.value(value)?;
        match self.builder.ty(value) {
            Type::Lgl | Type::Bool => Ok(self.truth(value)),
            _ => Err(Refusal::new(
                "a `Bool` function of something other than a Bool",
            )),
        }
    }

    /// A logical as a `Bool`, failing the call on `NA`.
    fn truth(&mut self, value: Local) -> Local {
        match self.builder.ty(value) {
            Type::Bool => value,
            _ => self.builder.emit(Type::Bool, Op::Truth(value)),
        }
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
        let preheader = self.builder.current;
        // The output's item type is the payload's, known once the function is lowered.
        let output = self.builder.local(Type::Null);
        let positions = self.positions(held);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, positions.counter));
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
            Expected::Free,
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
        self.next_position(positions);
        self.builder.switch_to(preheader);
        let zero = self.int(0);
        let fresh = self
            .builder
            .emit(Type::vector(payload), Op::NewVector(payload, zero));
        let fresh = self.builder.emit(output_ty, Op::Relabel(fresh, output_ty));
        self.builder.push(Some(output), Op::Copy(fresh));
        self.builder.switch_to(positions.exit);
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
        let (result, _) = self.lookup_result(dict, key, "KeyNotFound")?;
        Ok(result)
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
        self.usable(value)?;
        // A tag of another shape widens the dictionary's value union: its values are boxes
        // either way.
        let dict_ty = match (self.builder.ty(dict), self.builder.ty(value)) {
            (
                ty @ Type::Dict {
                    value: Item::Tag(_),
                    ..
                },
                Type::Tag(found),
            ) => with_tags(ty, found),
            (ty, _) => ty,
        };
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
        let item = Item::Record(shape);
        let zero = self.int(0);
        let fresh = self
            .builder
            .emit(Type::vector(item), Op::NewVector(item, zero));
        let ty = Type::Vector { item, array: true };
        let empty = self.builder.emit(ty, Op::Relabel(fresh, ty));
        let pairs = self.builder.emit(ty, Op::Copy(empty));
        let positions = self.positions(keys);
        let entry_key = self
            .builder
            .emit(Type::of_item(key), Op::ItemAt(keys, positions.counter));
        let entry_value = self
            .builder
            .emit(Type::of_item(value), Op::ItemAt(values, positions.counter));
        let pair = self.boxed(layout, false, &[entry_key, entry_value])?;
        let grown = self.builder.emit(ty, Op::Push(pairs, pair));
        self.builder.push(Some(pairs), Op::Copy(grown));
        self.next_position(positions);
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

    /// `Dict.from_list(pairs)`: each `(key, value)` pair inserted in turn into an empty
    /// dictionary, so a later pair's value replaces an earlier one's.
    unsafe fn dict_from_list(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [pairs] = arguments.exactly(1)?[..] else {
            return Err(Refusal::new("a malformed `Dict.from_list`"));
        };
        let (pairs, ty) = self.known_list(pairs, "Dict.from_list")?;
        let (layout, _) =
            interp::records::constructor_layout(None, &[rvalues::nil(), rvalues::nil()], false)
                .ok_or(Refusal::new("a pair the interpreter rejects"))?;
        let Some(Item::Record(shape)) = ty.items() else {
            return Err(Refusal::new(
                "a `Dict.from_list` of something other than pairs",
            ));
        };
        let held_shape = shapes::shape(shape);
        let (&[key @ (Item::Str | Item::I64 | Item::Dbl), value], true) =
            (&held_shape.fields[..], held_shape.layout == layout)
        else {
            return Err(Refusal::new(
                "a `Dict.from_list` of something other than pairs with `I64`, `F64` or `Str` keys",
            ));
        };
        let dict_ty = Type::Dict { key, value };
        let dict = self.builder.local(dict_ty);
        self.builder.push(Some(dict), Op::DictNew);
        let held = self.builder.emit(ty, Op::Copy(pairs));
        let positions = self.positions(held);
        let pair = self
            .builder
            .emit(Type::Record(shape), Op::ItemAt(held, positions.counter));
        let entry_key = self.field(pair, shape, 0)?;
        let entry_value = self.field(pair, shape, 1)?;
        let grown = self
            .builder
            .emit(dict_ty, Op::DictInsert(dict, entry_key, entry_value));
        self.builder.push(Some(dict), Op::Copy(grown));
        self.next_position(positions);
        Ok(dict)
    }

    /// The keys and values of `dict` as two ry lists in the dictionary's order, and their
    /// items. `dict` is a set for a `Set` function, which gives no list of values because
    /// every value is the same placeholder (`entry_value`).
    fn entries(
        &mut self,
        dict: Local,
        name: &str,
    ) -> Result<(Local, Option<Local>, Item, Item), Refusal> {
        let ty = self.builder.ty(dict);
        let (Some((key, value)), true) = (
            ty.entry_items(),
            matches!(ty, Type::Set(_)) == name.starts_with("Set."),
        ) else {
            return Err(Refusal(Cow::Owned(format!(
                "a `{name}` of something other than its collection"
            ))));
        };
        let keys = self.builder.emit(
            Type::Vector {
                item: key,
                array: true,
            },
            Op::DictKeys(dict),
        );
        let values = (!matches!(ty, Type::Set(_))).then(|| {
            self.builder.emit(
                Type::Vector {
                    item: value,
                    array: true,
                },
                Op::DictValues(dict),
            )
        });
        Ok((keys, values, key, value))
    }

    /// The value at `position` of the values `entries` gave, or a set's placeholder.
    fn entry_value(&mut self, values: Option<Local>, value: Item, position: Local) -> Local {
        match values {
            Some(values) => self
                .builder
                .emit(Type::of_item(value), Op::ItemAt(values, position)),
            None => self.set_member(),
        }
    }

    /// `Dict.join_map(dict, f)`: the entries of the dictionary `f` gives for each key and value,
    /// inserted in order into one fresh dictionary, in a loop with `f` inlined.
    /// `Set.join_map(set, f)` is the same loop over the set's elements.
    unsafe fn dict_join_map(
        &mut self,
        name: &str,
        arguments: &Arguments,
    ) -> Result<Local, Refusal> {
        let [dict, function] = arguments.exactly(2)?[..] else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        let dict = self.value(dict)?;
        let callable = self.function_argument(function)?;
        let (keys, values, key, value) = self.entries(dict, name)?;
        // The result's type is known only once the body is lowered, as for `Dict.map`.
        let preheader = self.builder.current;
        let output = self.builder.local(Type::Null);
        let positions = self.positions(keys);
        let entry_key = self
            .builder
            .emit(Type::of_item(key), Op::ItemAt(keys, positions.counter));
        let mut inputs: SmallVec<[Local; 2]> = SmallVec::from_slice(&[entry_key]);
        if let Some(values) = values {
            inputs.push(
                self.builder
                    .emit(Type::of_item(value), Op::ItemAt(values, positions.counter)),
            );
        }
        let inner = self
            .apply(callable, &inputs)?
            .ok_or(Refusal(Cow::Owned(format!(
                "a `{name}` callback that never completes"
            ))))?;
        let output_ty = self.builder.ty(inner);
        let (inner_keys, inner_values, inner_key, inner_value) = self.entries(inner, name)?;
        self.builder.function.locals[output.0 as usize] = output_ty;
        let inner_positions = self.positions(inner_keys);
        let joined_key = self.builder.emit(
            Type::of_item(inner_key),
            Op::ItemAt(inner_keys, inner_positions.counter),
        );
        let joined_value = self.entry_value(inner_values, inner_value, inner_positions.counter);
        let grown = self
            .builder
            .emit(output_ty, Op::DictInsert(output, joined_key, joined_value));
        self.builder.push(Some(output), Op::Copy(grown));
        self.next_position(inner_positions);
        self.next_position(positions);
        self.builder.switch_to(preheader);
        let fresh = self.builder.emit(output_ty, Op::DictNew);
        self.builder.push(Some(output), Op::Copy(fresh));
        self.builder.switch_to(positions.exit);
        Ok(output)
    }

    /// The list, dictionary or set of a capacity function, which gives it back as it is: native
    /// collections grow by doubling and keep no reserved room a program could read.
    fn capacity_operand(&mut self, name: &str, collection: Local) -> Result<Local, Refusal> {
        self.usable(collection)?;
        let fits = matches!(
            (
                name.split_once('.').map(|(module, _)| module),
                self.builder.ty(collection),
            ),
            (Some("List"), Type::Vector { array: true, .. })
                | (Some("Dict"), Type::Dict { .. })
                | (Some("Set"), Type::Set(_))
        );
        if !fits {
            return Err(Refusal(Cow::Owned(format!(
                "a `{name}` of something other than its collection"
            ))));
        }
        Ok(collection)
    }

    /// `Set.from_list(list)`: each item inserted in turn into an empty set, so a repeated item
    /// keeps its first position.
    unsafe fn set_from_list(&mut self, arguments: &Arguments) -> Result<Local, Refusal> {
        let [list] = arguments.exactly(1)?[..] else {
            return Err(Refusal::new("a malformed `Set.from_list`"));
        };
        let list = self.value(list)?;
        let ty = self.builder.ty(list);
        let Type::Vector {
            item: item @ (Item::I64 | Item::Str),
            array: true,
        } = ty
        else {
            return Err(Refusal::new(
                "a `Set.from_list` of something other than a list of whole numbers or strings",
            ));
        };
        let set_ty = Type::Set(item);
        let set = self.builder.local(set_ty);
        self.builder.push(Some(set), Op::DictNew);
        let held = self.builder.emit(ty, Op::Copy(list));
        let positions = self.positions(held);
        let element = self
            .builder
            .emit(Type::of_item(item), Op::ItemAt(held, positions.counter));
        let member = self.set_member();
        let grown = self
            .builder
            .emit(set_ty, Op::DictInsert(set, element, member));
        self.builder.push(Some(set), Op::Copy(grown));
        self.next_position(positions);
        Ok(set)
    }

    /// The element item of `set`, the operand of the set function `name`.
    fn set_item(&mut self, set: Local, name: &str) -> Result<Item, Refusal> {
        match self.builder.ty(set) {
            Type::Set(item) => Ok(item),
            _ => Err(Refusal(Cow::Owned(format!(
                "a `{name}` of something other than a set"
            )))),
        }
    }

    /// The placeholder a set's dictionary maps each element to.
    fn set_member(&mut self) -> Local {
        self.builder.emit(Type::Lgl, Op::Const(Constant::Lgl(1)))
    }

    /// `Dict.map(dict, f)`: each key with `f` of its key and value, inserted in order into a
    /// fresh dictionary, in a loop with `f` inlined. `Set.map(set, f)` inserts `f` of each
    /// element into a fresh set instead, so equal results merge.
    unsafe fn dict_map(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let [dict, function] = arguments.exactly(2)?[..] else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        let dict = self.value(dict)?;
        let callable = self.function_argument(function)?;
        let (keys, values, key, value) = self.entries(dict, name)?;
        let over_set = name == "Set.map";
        // The result's value type is known only once the body is lowered, so the output is made
        // in the preheader afterwards, as `List.map` makes its output.
        let preheader = self.builder.current;
        let output = self.builder.local(Type::Null);
        let positions = self.positions(keys);
        let entry_key = self
            .builder
            .emit(Type::of_item(key), Op::ItemAt(keys, positions.counter));
        let mut inputs: SmallVec<[Local; 2]> = SmallVec::from_slice(&[entry_key]);
        if let Some(values) = values {
            inputs.push(
                self.builder
                    .emit(Type::of_item(value), Op::ItemAt(values, positions.counter)),
            );
        }
        let mapped = self
            .apply(callable, &inputs)?
            .ok_or(Refusal(Cow::Owned(format!(
                "a `{name}` callback that never completes"
            ))))?;
        self.usable(mapped)?;
        let mapped_item = match (over_set, self.builder.ty(mapped).held_item()) {
            (true, Some(item @ (Item::Str | Item::I64))) | (false, Some(item)) => item,
            _ => {
                return Err(Refusal(Cow::Owned(format!(
                    "a `{name}` callback whose result its collection cannot hold"
                ))));
            }
        };
        let (output_ty, key, value) = if over_set {
            (Type::Set(mapped_item), mapped, self.set_member())
        } else {
            let output_ty = Type::Dict {
                key,
                value: mapped_item,
            };
            (output_ty, entry_key, mapped)
        };
        self.builder.function.locals[output.0 as usize] = output_ty;
        let grown = self
            .builder
            .emit(output_ty, Op::DictInsert(output, key, value));
        self.builder.push(Some(output), Op::Copy(grown));
        self.next_position(positions);
        self.builder.switch_to(preheader);
        let fresh = self.builder.emit(output_ty, Op::DictNew);
        self.builder.push(Some(output), Op::Copy(fresh));
        self.builder.switch_to(positions.exit);
        Ok(output)
    }

    /// `Dict.keep_if(dict, predicate)`, `Dict.drop_if`, `Dict.keep_shared(first, second)` and
    /// `Dict.remove_all(first, second)`: the entries that pass, inserted in order into a fresh
    /// dictionary, so the kept entries keep their order. A shared entry needs an equal value too,
    /// as the interpreter's `identical` compares it. The `Set` functions `keep_if`, `drop_if`,
    /// `intersection` and `difference` are the same loops, whose predicate takes the element.
    unsafe fn dict_filter(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let [dict, other] = arguments.exactly(2)?[..] else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        let dict = self.value(dict)?;
        let ty = self.builder.ty(dict);
        let by_predicate = matches!(
            name,
            "Dict.keep_if" | "Dict.drop_if" | "Set.keep_if" | "Set.drop_if"
        );
        let over_set = name.starts_with("Set.");
        let (callable, second) = if by_predicate {
            (Some(self.function_argument(other)?), None)
        } else {
            let second = self.value(other)?;
            if self.builder.ty(second) != ty {
                return Err(Refusal(Cow::Owned(format!(
                    "a `{name}` of collections of different types"
                ))));
            }
            (None, Some(second))
        };
        let (keys, values, key, value) = self.entries(dict, name)?;
        let output = self.builder.local(ty);
        self.builder.push(Some(output), Op::DictNew);
        let positions = self.positions(keys);
        let entry_key = self
            .builder
            .emit(Type::of_item(key), Op::ItemAt(keys, positions.counter));
        let entry_value = self.entry_value(values, value, positions.counter);
        let (keep, next) = (self.builder.block(), self.builder.block());
        match (callable, second) {
            (Some(callable), _) => {
                let inputs: &[Local] = if over_set {
                    &[entry_key]
                } else {
                    &[entry_key, entry_value]
                };
                let answer = self
                    .apply(callable, inputs)?
                    .ok_or(Refusal::new("a predicate that never completes"))?;
                let answer = match self.builder.ty(answer) {
                    Type::Bool => answer,
                    Type::Lgl => self.builder.emit(Type::Bool, Op::Truth(answer)),
                    _ => return Err(Refusal::new("a predicate whose answer is not a logical")),
                };
                let (then, otherwise) = if name.ends_with(".keep_if") {
                    (keep, next)
                } else {
                    (next, keep)
                };
                self.builder.terminate(Terminator::Branch {
                    condition: answer,
                    then,
                    otherwise,
                });
            }
            (None, Some(second)) => {
                let position = self
                    .builder
                    .emit(Type::I64, Op::DictFind(second, entry_key));
                let zero = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
                let present = self.test(Compare::Ge, Item::I64, position, zero);
                if matches!(name, "Dict.remove_all" | "Set.difference") {
                    self.builder.terminate(Terminator::Branch {
                        condition: present,
                        then: next,
                        otherwise: keep,
                    });
                } else if over_set {
                    self.builder.terminate(Terminator::Branch {
                        condition: present,
                        then: keep,
                        otherwise: next,
                    });
                } else {
                    let compare = self.builder.block();
                    self.builder.terminate(Terminator::Branch {
                        condition: present,
                        then: compare,
                        otherwise: next,
                    });
                    self.builder.switch_to(compare);
                    let theirs = self
                        .builder
                        .emit(Type::of_item(value), Op::DictAt(second, position));
                    let equal = self.identical_items(entry_value, theirs, name)?;
                    self.builder.terminate(Terminator::Branch {
                        condition: equal,
                        then: keep,
                        otherwise: next,
                    });
                }
            }
            (None, None) => return Err(Refusal(Cow::Owned(format!("a malformed `{name}`")))),
        }
        self.builder.switch_to(keep);
        let grown = self
            .builder
            .emit(ty, Op::DictInsert(output, entry_key, entry_value));
        self.builder.push(Some(output), Op::Copy(grown));
        self.builder.terminate(Terminator::Jump(next));
        self.builder.switch_to(next);
        self.next_position(positions);
        Ok(output)
    }

    /// `Dict.insert_all(first, second)`: each entry of `second` inserted in order into `first`,
    /// whose values for the same keys it replaces. `Set.union(first, second)` is the same loop,
    /// which leaves the elements of `first` first.
    unsafe fn dict_insert_all(
        &mut self,
        name: &str,
        arguments: &Arguments,
    ) -> Result<Local, Refusal> {
        let [first, second] = arguments.exactly(2)?[..] else {
            return Err(Refusal(Cow::Owned(format!("a malformed `{name}`"))));
        };
        let first = self.value(first)?;
        let second = self.value(second)?;
        let ty = self.builder.ty(first);
        if self.builder.ty(second) != ty {
            return Err(Refusal(Cow::Owned(format!(
                "a `{name}` of collections of different types"
            ))));
        }
        let (keys, values, key, value) = self.entries(second, name)?;
        let output = self.builder.local(ty);
        self.builder.push(Some(output), Op::Copy(first));
        let positions = self.positions(keys);
        let entry_key = self
            .builder
            .emit(Type::of_item(key), Op::ItemAt(keys, positions.counter));
        let entry_value = self.entry_value(values, value, positions.counter);
        let grown = self
            .builder
            .emit(ty, Op::DictInsert(output, entry_key, entry_value));
        self.builder.push(Some(output), Op::Copy(grown));
        self.next_position(positions);
        Ok(output)
    }

    /// `:Ok(value)` when `dict` holds `key`, else `:Err(:missing)`, and the position
    /// [`Op::DictFind`] gave for the key.
    unsafe fn lookup_result(
        &mut self,
        dict: Local,
        key: Local,
        missing: &str,
    ) -> Result<(Local, Local), Refusal> {
        let Type::Dict { value: item, .. } = self.builder.ty(dict) else {
            return Err(Refusal::new(
                "a lookup in something other than a dictionary",
            ));
        };
        let position = self.builder.emit(Type::I64, Op::DictFind(dict, key));
        let first = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
        let contains = self.builder.emit(
            Type::Bool,
            Op::Compare(Compare::Ge, Item::I64, position, first),
        );
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
            .emit(Type::of_item(item), Op::DictAt(dict, position));
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
        Ok((result, position))
    }

    /// `Dict.update(dict, key, f)`: `f` gets `:Ok(value)` or `:Err(:Missing)`, and an `:Ok` it
    /// gives sets the key while an `:Err` removes it.
    ///
    /// A function written at the call is applied once where the key is found and once where it
    /// is missing, not once to a result joined from the two. Its argument and usually its answer
    /// are then each built by one instruction and only read, so [`crate::unbox`] removes both
    /// boxes, and the update of a counter or a running total allocates nothing. A typed closure
    /// called directly takes the joined result, so it is compiled for the one type it declares.
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
        if let Callable::Typed(..) = callable {
            let (current, position) = self.lookup_result(dict, key, "Missing")?;
            let updated = self
                .store_update(callable, current, dict, key, position, None)?
                .ok_or(Refusal::new("an update that never completes"))?;
            return Ok(updated);
        }
        let position = self.builder.emit(Type::I64, Op::DictFind(dict, key));
        let first = self.builder.emit(Type::I64, Op::Const(Constant::I64(0)));
        let contains = self.builder.emit(
            Type::Bool,
            Op::Compare(Compare::Ge, Item::I64, position, first),
        );
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
        let result = self.builder.local(dict_ty);
        let mut completes = false;
        for present in [true, false] {
            self.builder.switch_to(if present { found } else { absent });
            let current = if present {
                let value = self
                    .builder
                    .emit(Type::of_item(item), Op::DictAt(dict, position));
                self.tag("Ok", &[value])?
            } else {
                let reason = self.tag("Missing", &[])?;
                self.tag("Err", &[reason])?
            };
            let updated =
                self.store_update(callable, current, dict, key, position, Some(present))?;
            if let Some(updated) = updated {
                self.builder.push(Some(result), Op::Copy(updated));
                self.builder.terminate(Terminator::Jump(join));
                completes = true;
            }
        }
        if !completes {
            return Err(Refusal::new("an update that never completes"));
        }
        self.builder.switch_to(join);
        Ok(result)
    }

    /// Applies the function of a `Dict.update` to `current` and gives `dict` with its answer
    /// stored under `key`, or `None` when the function never completes. `present` says whether
    /// the dictionary is known to hold the key, at `position`, or known not to hold it; `None`
    /// leaves that to `position`, which is negative for a missing key.
    unsafe fn store_update(
        &mut self,
        callable: Callable,
        current: Local,
        dict: Local,
        key: Local,
        position: Local,
        present: Option<bool>,
    ) -> Result<Option<Local>, Refusal> {
        let dict_ty = self.builder.ty(dict);
        let Type::Dict { value: item, .. } = dict_ty else {
            return Err(Refusal::new(
                "a `Dict.update` of something other than a dictionary",
            ));
        };
        let Some(answer) = self.apply(callable, &[current])? else {
            return Ok(None);
        };
        let (ok, err) = (tag_layout("Ok", 1)?, tag_layout("Err", 1)?);
        let (candidates, exhaustive) = self.shapes_of(self.builder.ty(answer), &[ok, err], &[])?;
        let updated = self.switch_shapes(
            answer,
            &candidates,
            exhaustive,
            Expected::Free,
            &mut |lowering, shape| {
                let layout = shape.map(|shape| shapes::shape(shape).layout);
                let op = match (shape, layout) {
                    (Some(shape), Some(layout)) if layout == ok => {
                        let value = lowering.field(answer, shape, 0)?;
                        // A dictionary's values all have its value type, which `Dict.update`
                        // checks rather than converts, as `Dict.insert` does.
                        if lowering.convert_scalar(value, item)? != value {
                            return Err(Refusal::new("an update to a value of another type"));
                        }
                        match present {
                            Some(false) => Op::DictInsert(dict, key, value),
                            _ => Op::DictPut(dict, key, position, value),
                        }
                    }
                    (_, Some(layout)) if layout == err => match present {
                        Some(false) => Op::Copy(dict),
                        _ => Op::DictRemove(dict, key),
                    },
                    _ => {
                        lowering.builder.terminate(Terminator::Fail);
                        let unreachable = lowering.builder.block();
                        lowering.builder.switch_to(unreachable);
                        return Ok(None);
                    }
                };
                Ok(Some(lowering.builder.emit(dict_ty, op)))
            },
        )?;
        if let Some(updated) = updated {
            self.usable(updated)?;
        }
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
        let parses = self.builder.local(Type::Bool);
        let number = self.builder.emit(
            ty,
            Op::Parse {
                string,
                parsed: parses,
            },
        );
        self.checked_result(parses, number, "InvalidNumStr")
    }

    /// `Str.from_utf8`, `Str.from_utf16` or `Str.from_utf32`, or a lossy form.
    unsafe fn decode(&mut self, name: &str, arguments: &Arguments) -> Result<Local, Refusal> {
        let [units] = arguments.exactly(1)?[..] else {
            return Err(Refusal::new("a malformed `Str` function"));
        };
        let units = self.value(units)?;
        let numbers = Type::Vector {
            item: Item::I64,
            array: true,
        };
        self.settle(units, Item::I64)?;
        if self.builder.ty(units) != numbers {
            return Err(Refusal::new(
                "a decoding of something other than a list of I64s",
            ));
        }
        let (encoding, reason) = match name {
            "Str.from_utf8" | "Str.from_utf8_lossy" => (Encoding::Utf8, "BadUtf8"),
            "Str.from_utf16" | "Str.from_utf16_lossy" => (Encoding::Utf16, "BadUtf16"),
            _ => (Encoding::Utf32, "BadUtf32"),
        };
        if name.ends_with("_lossy") {
            return Ok(self.builder.emit(
                Type::Str,
                Op::Decode {
                    encoding,
                    units,
                    valid: None,
                },
            ));
        }
        let valid = self.builder.local(Type::Bool);
        let text = self.builder.emit(
            Type::Str,
            Op::Decode {
                encoding,
                units,
                valid: Some(valid),
            },
        );
        self.checked_result(valid, text, reason)
    }

    /// `:Ok(value)` when `valid`, else `:Err(:<reason>)`.
    unsafe fn checked_result(
        &mut self,
        valid: Local,
        value: Local,
        reason: &str,
    ) -> Result<Local, Refusal> {
        let (then, otherwise, join) = (
            self.builder.block(),
            self.builder.block(),
            self.builder.block(),
        );
        self.builder.terminate(Terminator::Branch {
            condition: valid,
            then,
            otherwise,
        });
        self.builder.switch_to(then);
        let ok = self.tag("Ok", &[value])?;
        self.builder.switch_to(otherwise);
        let reason = self.tag(reason, &[])?;
        let err = self.tag("Err", &[reason])?;
        let Some(Type::Tag(union)) = joined(self.builder.ty(ok), self.builder.ty(err)) else {
            return Err(Refusal::new("a checked result"));
        };
        let result = self.builder.local(Type::Tag(union));
        self.builder.push(Some(result), Op::Copy(err));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(then);
        self.builder.push(Some(result), Op::Copy(ok));
        self.builder.terminate(Terminator::Jump(join));
        self.builder.switch_to(join);
        Ok(result)
    }

    /// The code units of a string in an encoding, as a list of `I64`s.
    fn encode(&mut self, encoding: Encoding, string: Local) -> Local {
        self.builder.emit(
            Type::Vector {
                item: Item::I64,
                array: true,
            },
            Op::Encode(encoding, string),
        )
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
            let Some(speculation) = find_function(head, self.env) else {
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
        // Records, tuples, tags and lists as items compare structurally, and only for equality.
        let object = |item: Item| matches!(item, Item::Record(_) | Item::Tag(_) | Item::Array(_));
        if object(ia) || object(ib) {
            if !(object(ia) && object(ib)) || !matches!(op, Compare::Eq | Compare::Ne) {
                return Err(Refusal::new(
                    "an ordering of records, tags or lists, or one beside another value",
                ));
            }
            if !vectors {
                return Ok(self.builder.emit(Type::Bool, Op::Compare(op, ia, a, b)));
            }
            let result = Type::Vector {
                item: Item::Lgl,
                array: true,
            };
            return Ok(self.builder.emit(result, Op::VectorCompare(op, ia, a, b)));
        }
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

/// A dictionary's or a set's type from its annotation, such as `Dict<Str, F64>` or `Set<I64>`.
/// A dictionary's values may be any item, spelled as [`spelled_item`] reads it.
unsafe fn keyed_type(annotation: &str) -> Option<Type> {
    let key = |name: &str| match name.trim() {
        "Str" => Some(Item::Str),
        "I64" => Some(Item::I64),
        "F64" => Some(Item::Dbl),
        _ => None,
    };
    let annotation = annotation.trim();
    if let Some(inner) = annotation.strip_prefix("Set<") {
        return match key(inner.strip_suffix('>')?)? {
            Item::Dbl => None,
            element => Some(Type::Set(element)),
        };
    }
    let inner = annotation.strip_prefix("Dict<")?.strip_suffix('>')?;
    let mut parts = top_level_parts(inner);
    let (Some(key_text), Some(value_text), None) = (parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    Some(Type::Dict {
        key: key(key_text)?,
        value: spelled_item(value_text)?,
    })
}

/// The item of a value of the type `text` spells as a canonical signature does, such as
/// `Record{x: I64, ys: List<F64>}`. Every tag type is any tag, as a box's tag field is.
unsafe fn spelled_item(text: &str) -> Option<Item> {
    let text = text.trim();
    if text.starts_with(':') {
        return Some(Item::Tag(shapes::ANY));
    }
    match text {
        "I64" => return Some(Item::I64),
        "F64" => return Some(Item::Dbl),
        "Str" => return Some(Item::Str),
        "Bool" => return Some(Item::Lgl),
        _ => {}
    }
    if let Some(inner) = text
        .strip_prefix("List<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        return Some(Item::Array(shapes::array_of(spelled_item(inner)?)));
    }
    if let Some(inner) = text
        .strip_prefix("Tuple{")
        .and_then(|rest| rest.strip_suffix('}'))
    {
        let items = top_level_parts(inner)
            .map(|part| spelled_item(part))
            .collect::<Option<SmallVec<[Item; 4]>>>()?;
        let names: SmallVec<[Sexp; 4]> = items.iter().map(|_| rvalues::nil()).collect();
        let (layout, _) = interp::records::constructor_layout(None, &names, false)?;
        return shapes::intern(layout, &items).map(Item::Record);
    }
    let inner = text.strip_prefix("Record{")?.strip_suffix('}')?;
    let mut names: SmallVec<[Sexp; 4]> = SmallVec::new();
    let mut items: SmallVec<[Item; 4]> = SmallVec::new();
    for field in top_level_parts(inner) {
        let (name, ty) = field.split_once(':')?;
        names.push(rvalues::install(name.trim()).ok()?);
        items.push(spelled_item(ty)?);
    }
    let (layout, slots) = interp::records::constructor_layout(None, &names, true)?;
    let mut placed: SmallVec<[(usize, Item); 4]> = slots.into_iter().zip(items).collect();
    placed.sort_by_key(|&(slot, _)| slot);
    let fields: SmallVec<[Item; 4]> = placed.into_iter().map(|(_, item)| item).collect();
    shapes::intern(layout, &fields).map(Item::Record)
}

/// The comma-separated parts of a type's spelling that no bracket encloses.
fn top_level_parts(text: &str) -> impl Iterator<Item = &str> {
    let mut depth = 0usize;
    let mut start = 0;
    let mut parts: SmallVec<[&str; 4]> = SmallVec::new();
    for (at, byte) in text.bytes().enumerate() {
        match byte {
            b'<' | b'{' | b'(' => depth += 1,
            b'>' | b'}' | b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(&text[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts.into_iter()
}

/// A `match` arm: its pattern, its `if` guard and its value.
#[derive(Clone, Copy)]
struct MatchArm {
    pattern: Sexp,
    guard: Option<Sexp>,
    body: Sexp,
}

/// What the lowering knows of the shape of a value a pattern tests.
#[derive(Clone, Copy)]
enum Known {
    /// A box of this shape.
    Shape(u32),
    /// A box of a shape that no constructor pattern at the top of an arm names.
    Other,
    /// Only its type.
    Typed,
}

/// Whether a pattern matches, given the tests lowered for it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Test {
    /// Whatever the value: nothing is tested.
    Always,
    /// Where its tests pass, which is where the builder is.
    Maybe,
    /// Never: the code jumps to where it fails.
    Never,
}

/// What a `match` pattern of a tuple, record or tag names: a layout and each item's slot, for
/// `:Name(..)` only the tag, and for a record or a tag's named payload with a trailing `..` the
/// tag and the field names it needs among others.
enum PatternLayout {
    Exact(u32, Vec<usize>),
    Tag(u32),
    Open { tag: Option<u32>, names: Vec<Sexp> },
}

/// The layout a constructor pattern `.ry_tag(...)`, `.ry_record(...)` or `.ry_tuple(...)`
/// tests, with its items, a trailing `..` left out.
unsafe fn pattern_layout(pattern: Sexp) -> Result<(PatternLayout, Vec<(Sexp, Sexp)>), Refusal> {
    let head = rvalues::symbol_name(pattern.car());
    let items = rvalues::pairlist_items(pattern.cdr());
    let (name, payload) = if head == ".ry_tag" {
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
    let tag = match name {
        Some(name) => {
            let text = rvalues::string_at(name, 0).ok_or(Refusal::new("a tag pattern's name"))?;
            let symbol = rvalues::install(&text).map_err(|_| Refusal::new("a tag name"))?;
            Some(interp::heap().layouts.intern_tag(symbol))
        }
        None => None,
    };
    let open = payload
        .last()
        .is_some_and(|&(item_tag, item)| item_tag == rvalues::nil() && rest(item));
    let fields = &payload[..payload.len() - usize::from(open)];
    if fields.iter().any(|&(_, item)| rest(item)) {
        return Err(Refusal::new("a pattern with `..` among its fields"));
    }
    if open {
        if let (Some(tag), []) = (tag, fields) {
            return Ok((PatternLayout::Tag(tag), Vec::new()));
        }
        if head == ".ry_tuple" || fields.iter().any(|&(tag, _)| tag == rvalues::nil()) {
            return Err(Refusal::new("a positional pattern with `..`"));
        }
        let names = fields.iter().map(|&(tag, _)| tag).collect();
        return Ok((PatternLayout::Open { tag, names }, fields.to_vec()));
    }
    let names: Vec<Sexp> = payload.iter().map(|&(tag, _)| tag).collect();
    let (layout, slots) = interp::records::constructor_layout(name, &names, head == ".ry_record")
        .ok_or(Refusal::new("a pattern the interpreter rejects"))?;
    Ok((PatternLayout::Exact(layout, slots), payload))
}

/// The slot of each field `constructor` reads when it matches a box of `shape`, or `None` when it
/// does not match one.
unsafe fn constructor_fits(constructor: &PatternLayout, shape: u32) -> Option<Vec<usize>> {
    let found = shapes::shape(shape).layout;
    match constructor {
        PatternLayout::Exact(layout, slots) => (*layout == found).then(|| slots.clone()),
        PatternLayout::Tag(tag) => {
            (interp::heap().layouts.get(found)?.tag == Some(*tag)).then(Vec::new)
        }
        PatternLayout::Open { tag, names } => {
            let layout = interp::heap().layouts.get(found)?;
            let Fields::Named(fields) = &layout.fields else {
                return None;
            };
            if layout.tag != *tag {
                return None;
            }
            names
                .iter()
                .map(|name| fields.iter().position(|field| field == name))
                .collect()
        }
    }
}

/// Adds the layouts the constructor patterns at the top of `pattern` name, and the tags they
/// name with any payload, which a `match` over any tag tests for. `true` when one of them names
/// only some of its fields, which no layout stands for.
unsafe fn pattern_layouts(
    pattern: Sexp,
    layouts: &mut SmallVec<[u32; 4]>,
    tags: &mut SmallVec<[u32; 4]>,
) -> Result<(), Refusal> {
    if pattern.kind() != SexpType::Language || pattern.car().kind() != SexpType::Symbol {
        return Ok(());
    }
    let items = rvalues::pairlist_items(pattern.cdr());
    match &*rvalues::symbol_name(pattern.car()) {
        "(" | "|" => {
            for (_, inner) in items {
                pattern_layouts(inner, layouts, tags)?;
            }
        }
        ".ry_as" => {
            if let Some(&(_, inner)) = items.first() {
                pattern_layouts(inner, layouts, tags)?;
            }
        }
        ".ry_tag" | ".ry_record" | ".ry_tuple" => match pattern_layout(pattern)?.0 {
            PatternLayout::Exact(layout, _) => {
                if !layouts.contains(&layout) {
                    layouts.push(layout);
                }
            }
            PatternLayout::Tag(tag) | PatternLayout::Open { tag: Some(tag), .. } => {
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
            PatternLayout::Open { tag: None, .. } => {}
        },
        _ => {}
    }
    Ok(())
}

/// Whether `pattern` is `_`, which reads nothing.
unsafe fn is_wildcard(pattern: Sexp) -> bool {
    pattern.kind() == SexpType::Symbol && &*rvalues::symbol_name(pattern) == "_"
}

/// Whether `expression` assigns a name, has a `for` loop or a `match`, local functions included.
unsafe fn writes(expression: Sexp) -> bool {
    if expression.kind() != SexpType::Language {
        return false;
    }
    let head = expression.car();
    if head.kind() == SexpType::Symbol
        && matches!(
            &*rvalues::symbol_name(head),
            "<-" | "=" | "<<-" | "->" | "->>" | "for" | "assign" | ".ry_match"
        )
    {
        return true;
    }
    writes(head)
        || rvalues::pairlist_items(expression.cdr())
            .into_iter()
            .any(|(_, item)| writes(item))
}

/// The values of `arguments`, or `None` when one is a function.
fn plain_values(arguments: &[Argument]) -> Option<SmallVec<[Local; 4]>> {
    arguments
        .iter()
        .map(|&argument| match argument {
            Argument::Value(value) => Some(value),
            Argument::Function(_) => None,
        })
        .collect()
}

thread_local! {
    /// The function of each library function and arity that [`eta`] made, kept for the session.
    static ETA: RefCell<Vec<(Sexp, usize, Sexp, Sexp)>> = const { RefCell::new(Vec::new()) };
}

/// For each of `formals`, the index of the argument among `items` that supplies it, matched as R
/// matches a call: by exact name, then by a prefix that fits one formal only, then the unnamed
/// arguments in order. A call R would reject, and a function with `...`, refuse.
pub unsafe fn match_formals(
    formals: &[(Sexp, Sexp)],
    items: &[(Option<String>, Sexp)],
) -> Result<SmallVec<[Option<usize>; 4]>, Refusal> {
    if formals
        .iter()
        .any(|&(symbol, _)| symbol == rvalues::symbols().dots)
    {
        return Err(Refusal::new(
            "a typed function it calls does not compile: a function with `...`",
        ));
    }
    let unmatched = || Refusal::new("a call whose arguments R would not match to the formals");
    let names: SmallVec<[Cow<'_, str>; 4]> = formals
        .iter()
        .map(|&(symbol, _)| rvalues::symbol_name(symbol))
        .collect();
    let mut supplied: SmallVec<[Option<usize>; 4]> = SmallVec::from_elem(None, formals.len());
    let mut partial: SmallVec<[usize; 4]> = SmallVec::new();
    for (item, (tag, _)) in items.iter().enumerate() {
        let Some(tag) = tag else { continue };
        match names.iter().position(|name| name == tag) {
            Some(formal) if supplied[formal].is_none() => supplied[formal] = Some(item),
            Some(_) => return Err(unmatched()),
            None => partial.push(item),
        }
    }
    // A prefix matches only the formals no exact name took.
    let exact = supplied.clone();
    for item in partial {
        let Some(tag) = items[item].0.as_deref() else {
            continue;
        };
        let mut fits = (0..formals.len())
            .filter(|&formal| exact[formal].is_none() && names[formal].starts_with(tag));
        match (fits.next(), fits.next()) {
            (Some(formal), None) if supplied[formal].is_none() => supplied[formal] = Some(item),
            _ => return Err(unmatched()),
        }
    }
    let open: SmallVec<[usize; 4]> = (0..formals.len())
        .filter(|&formal| supplied[formal].is_none())
        .collect();
    let mut open = open.into_iter();
    for (item, (tag, _)) in items.iter().enumerate() {
        if tag.is_none() {
            supplied[open.next().ok_or_else(unmatched)?] = Some(item);
        }
    }
    Ok(supplied)
}

/// Where the defaults of a called function are computed.
#[derive(Clone, Copy)]
enum Frame {
    /// A local function's: the first so many scopes and its body.
    Local(usize, Sexp),
    /// A typed closure's, in its own environment.
    Typed(Sexp),
}

/// Whether a default computed at the call may call `name` besides the standard library: each
/// gives a value and has no effect but an error.
fn is_ahead_operator(name: &str) -> bool {
    matches!(
        name,
        "+" | "-"
            | "*"
            | "/"
            | "^"
            | "%%"
            | "%/%"
            | "<"
            | ">"
            | "<="
            | ">="
            | "=="
            | "!="
            | "&"
            | "&&"
            | "|"
            | "||"
            | "!"
            | "("
            | ".ry_array"
            | ".ry_record"
            | ".ry_tuple"
            | ".ry_tag"
    )
}

/// The value of the function or constant of ry's standard library that `symbol` finds from
/// `env`, such as `Num.abs` or `Num.pi`, unless a binding of the program hides it.
unsafe fn std_binding(symbol: Sexp, env: Sexp) -> Option<Sexp> {
    let original = interp::standard_library::builtins_env()
        .and_then(|builtins| interp::heap().frame_cell(builtins, symbol))
        .map(|cell| cell.cdr())?;
    resolve_value(symbol, env)
        .is_some_and(|speculation| speculation.function == original)
        .then_some(original)
}

/// The symbol a call's head names: `f` in `f(...)`, and `f` in `pkg::f(...)` or `pkg:::f(...)`.
unsafe fn called_symbol(head: Sexp) -> Option<Sexp> {
    match head.kind() {
        SexpType::Symbol => Some(head),
        SexpType::Language
            if head.car().kind() == SexpType::Symbol
                && matches!(&*rvalues::symbol_name(head.car()), "::" | ":::") =>
        {
            match rvalues::pairlist_items(head.cdr())[..] {
                [_, (_, symbol)] if symbol.kind() == SexpType::Symbol => Some(symbol),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Whether `function` is base R's function `symbol`, as [`is_base_function`] tells for a
/// lookup.
unsafe fn is_base_value(symbol: Sexp, function: Sexp) -> bool {
    match function.kind() {
        SexpType::Builtin | SexpType::Special => interp::eval::builtin_def(function)
            .is_ok_and(|def| rvalues::symbol_name(symbol) == def.name),
        SexpType::Closure => {
            let heap = interp::heap();
            heap.frame_cell(heap.known().base_namespace, symbol)
                .is_some_and(|cell| {
                    let value = cell.cdr();
                    value == function
                        || value.kind() == SexpType::Promise && value.car() == function
                })
        }
        _ => false,
    }
}

/// The formals and body of `function(a1, ..., an) name(a1, ..., an)` for the library function
/// `name` passed by name, which lowers as a call of it with the values passed. The language
/// objects are preserved, so each is made once per session.
unsafe fn eta(name: Sexp, arity: usize) -> Result<(Sexp, Sexp), Refusal> {
    if let Some(found) = ETA.with_borrow(|made| {
        made.iter()
            .find(|&&(held, count, ..)| held == name && count == arity)
            .map(|&(.., formals, body)| (formals, body))
    }) {
        return Ok(found);
    }
    let failed = |_| Refusal::new("a library function passed by name");
    let mut formals = rvalues::nil();
    let mut arguments = rvalues::nil();
    for position in (1..=arity).rev() {
        let symbol = rvalues::install(&format!(".ry_passed{position}")).map_err(failed)?;
        formals = rvalues::cons_tagged(rvalues::missing_arg(), formals, symbol).map_err(failed)?;
        arguments = rvalues::cons(symbol, arguments).map_err(failed)?;
    }
    let body = rvalues::lcons(name, arguments).map_err(failed)?;
    let heap = interp::heap();
    heap.preserve(formals);
    heap.preserve(body);
    ETA.with_borrow_mut(|made| made.push((name, arity, formals, body)));
    Ok((formals, body))
}

/// A typed function specialized for the functions a call passes it ([`specialization`]).
struct Specialization {
    function: Sexp,
    passed: PassedFunctions,
    synthetic: Sexp,
}

thread_local! {
    /// The specializations made, kept for the session, at most [`MAX_SPECIALIZATIONS`].
    static SPECIALIZATIONS: RefCell<Vec<Specialization>> = const { RefCell::new(Vec::new()) };
}

/// How many specializations of typed functions for passed functions a session makes. A
/// function that passes a different literal to itself at each level would make one per level.
const MAX_SPECIALIZATIONS: usize = 256;

/// The specialization of `function` for `passed`, when one was made.
fn existing_specialization(function: Sexp, passed: &PassedFunctions) -> Option<Sexp> {
    SPECIALIZATIONS.with_borrow(|made| {
        made.iter()
            .find(|held| held.function == function && held.passed == *passed)
            .map(|held| held.synthetic)
    })
}

/// The typed closure that runs `function` with the functions of `passed` in place of their
/// parameters, made once per function and passed functions. Compiled code holds no function
/// values, so a recursive function that passes a function on to itself cannot lower the
/// function into each call. Its specialization takes the other parameters, then one parameter
/// per value a passed literal reads from its caller, and binds each function parameter to its
/// function before the body runs ([`specialized_bindings`]). A recursive call that passes the
/// same functions on is then the specialization calling itself.
///
/// The closure is `function`'s body over the remaining formals in `function`'s environment,
/// with `function`'s signature for them, so it is ordinary R that only the typed core runs.
unsafe fn specialization(function: Sexp, passed: &PassedFunctions) -> Result<Sexp, Refusal> {
    if let Some(synthetic) = existing_specialization(function, passed) {
        return Ok(synthetic);
    }
    if SPECIALIZATIONS.with_borrow(Vec::len) >= MAX_SPECIALIZATIONS {
        return Err(Refusal::new(
            "the session made its limit of typed functions specialized for passed functions",
        ));
    }
    let failed = |_| Refusal::new("a typed function specialized for a passed function");
    let carried = interp::annotations::stream_signature(function)
        .map_err(failed)?
        .ok_or(Refusal::new("a passed function's callee has no signature"))?;
    let (masks, texts) = (carried.element(0), carried.element(1));
    let formals = rvalues::pairlist_items(function.car());
    if masks.kind() != SexpType::Integer
        || rvalues::length(masks) != FIRST_SLOT + formals.len()
        || rvalues::length(texts) != rvalues::length(masks)
    {
        return Err(Refusal::new(
            "a passed function's callee has another signature",
        ));
    }
    let mut kept: Vec<(Sexp, i32, String)> = vec![];
    for slot in 0..FIRST_SLOT {
        let text = rvalues::string_at(texts, slot).unwrap_or_default();
        kept.push((
            rvalues::nil(),
            masks.slice::<i32>()[slot],
            text.into_owned(),
        ));
    }
    for (position, &(symbol, _)) in formals.iter().enumerate() {
        if passed
            .iter()
            .any(|&(held, ..)| held == PassedAt::Formal(position))
        {
            continue;
        }
        let slot = FIRST_SLOT + position;
        let text = rvalues::string_at(texts, slot).unwrap_or_default();
        kept.push((symbol, masks.slice::<i32>()[slot], text.into_owned()));
    }
    let mut captures = 0;
    for (.., names) in passed {
        for _ in names {
            captures += 1;
            let symbol = rvalues::install(&format!(".ry_captured{captures}")).map_err(failed)?;
            kept.push((symbol, 0, String::new()));
        }
    }
    if kept.len() == FIRST_SLOT {
        return Err(Refusal::new(
            "a function passed to a recursive typed function that takes nothing else",
        ));
    }
    let mut synthetic_formals = rvalues::nil();
    for &(symbol, ..) in kept[FIRST_SLOT..].iter().rev() {
        synthetic_formals = rvalues::cons_tagged(rvalues::missing_arg(), synthetic_formals, symbol)
            .map_err(failed)?;
    }
    let heap = interp::heap();
    heap.preserve(synthetic_formals);
    let body = interp::bytecode::closure_body(function);
    let synthetic = rvalues::triple(SexpType::Closure, synthetic_formals, body, function.tag())
        .map_err(failed)?;
    heap.preserve(synthetic);
    let masks: Vec<i32> = kept.iter().map(|&(_, mask, _)| mask).collect();
    let texts: Vec<&str> = kept.iter().map(|(.., text)| text.as_str()).collect();
    let names: Vec<String> = kept[FIRST_SLOT..]
        .iter()
        .map(|&(symbol, ..)| rvalues::symbol_name(symbol).into_owned())
        .collect();
    let masks = rvalues::integer_vector(&masks).map_err(failed)?;
    heap.preserve(masks);
    let texts = rvalues::string_vector(&texts).map_err(failed)?;
    heap.preserve(texts);
    let names = rvalues::string_vector(&names).map_err(failed)?;
    heap.preserve(names);
    let signature = rvalues::list_vector(&[masks, texts, names]).map_err(failed);
    for object in [masks, texts, names] {
        heap.release(object);
    }
    interp::annotations::restore_signature(synthetic, signature?).map_err(failed)?;
    // The literals' code outlives the caller that holds it once the specialization is made.
    heap.preserve(function);
    for &(_, passed, _) in passed {
        let (PassedFunction::Literal(first, second) | PassedFunction::Typed(first, second)) =
            passed
        else {
            continue;
        };
        heap.preserve(first);
        heap.preserve(second);
    }
    SPECIALIZATIONS.with_borrow_mut(|made| {
        made.push(Specialization {
            function,
            passed: passed.clone(),
            synthetic,
        })
    });
    Ok(synthetic)
}

/// Where [`specialized_bindings`] binds a passed function: to a formal's symbol, or to a name
/// that an earlier passed literal reads.
enum SpecializedAt {
    Formal(Sexp),
    Read { owner: usize, name: Sexp },
}

/// For the specialization `closure`, each function the call passed, with where it is bound
/// and, for a literal, the names it reads with the parameters that pass their values
/// ([`specialization`]). `None` when `closure` is no specialization.
#[allow(clippy::type_complexity)]
unsafe fn specialized_bindings(
    closure: Sexp,
) -> Result<
    Option<SmallVec<[(SpecializedAt, PassedFunction, SmallVec<[(Sexp, Sexp); 2]>); 1]>>,
    Refusal,
> {
    let Some((function, passed)) = SPECIALIZATIONS.with_borrow(|made| {
        made.iter()
            .find(|held| held.synthetic == closure)
            .map(|held| (held.function, held.passed.clone()))
    }) else {
        return Ok(None);
    };
    let failed = |_| Refusal::new("a typed function specialized for a passed function");
    let formals = rvalues::pairlist_items(function.car());
    let mut captures = 0;
    let mut bindings = SmallVec::new();
    for (at, passed, names) in passed {
        let at = match at {
            PassedAt::Formal(position) => SpecializedAt::Formal(
                formals
                    .get(position)
                    .ok_or(Refusal::new(
                        "a specialization of another function's formals",
                    ))?
                    .0,
            ),
            PassedAt::Read { owner, name } => SpecializedAt::Read { owner, name },
        };
        let mut reads = SmallVec::new();
        for name in names {
            captures += 1;
            let formal = rvalues::install(&format!(".ry_captured{captures}")).map_err(failed)?;
            reads.push((name, formal));
        }
        bindings.push((at, passed, reads));
    }
    Ok(Some(bindings))
}

/// The symbols `code` reads or names, each once in the order it first appears, leaving out the
/// field names of `$` and `@`.
unsafe fn free_symbols(code: Sexp, out: &mut SmallVec<[Sexp; 4]>) {
    match code.kind() {
        SexpType::Symbol => {
            if code != rvalues::missing_arg() && !out.contains(&code) {
                out.push(code);
            }
        }
        SexpType::Language => {
            let head = code.car();
            let items = rvalues::pairlist_items(code.cdr());
            if head.kind() == SexpType::Symbol && matches!(&*rvalues::symbol_name(head), "$" | "@")
            {
                if let Some(&(_, object)) = items.first() {
                    free_symbols(object, out);
                }
                return;
            }
            free_symbols(head, out);
            for (_, item) in items {
                free_symbols(item, out);
            }
        }
        SexpType::Pairlist => {
            for (_, item) in rvalues::pairlist_items(code) {
                free_symbols(item, out);
            }
        }
        _ => {}
    }
}

/// The tags a value of `ty` holds: a tag's union, or the item union of a ry list of tags or of
/// a dictionary's tag values.
fn held_tags(ty: Type) -> Option<u32> {
    match ty {
        Type::Tag(union)
        | Type::Vector {
            item: Item::Tag(union),
            array: true,
        }
        | Type::Dict {
            value: Item::Tag(union),
            ..
        } => Some(union),
        _ => None,
    }
}

/// `ty` holding the tags of `union` as well, for a type [`held_tags`] reads.
fn with_tags(ty: Type, union: u32) -> Type {
    match ty {
        Type::Tag(held) => Type::Tag(shapes::join(held, union)),
        Type::Vector {
            item: Item::Tag(held),
            array: true,
        } => Type::Vector {
            item: Item::Tag(shapes::join(held, union)),
            array: true,
        },
        Type::Dict {
            key,
            value: Item::Tag(held),
        } => Type::Dict {
            key,
            value: Item::Tag(shapes::join(held, union)),
        },
        ty => ty,
    }
}

/// The type both of two values' types fit: that type, or the union of two tags or two ry lists
/// of tags.
fn joined(a: Type, b: Type) -> Option<Type> {
    if let (Some(left), Some(right)) = (held_tags(a), held_tags(b)) {
        return (with_tags(a, right) == with_tags(b, left)).then(|| with_tags(a, right));
    }
    match (a, b) {
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

/// A counted loop: its `I64` counter, the last value the counter takes, and the blocks of its
/// header and its exit.
#[derive(Clone, Copy)]
struct Positions {
    counter: Local,
    end: Local,
    header: BlockId,
    exit: BlockId,
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

/// The mapping calls whose empty result the native code builds when a `.ry_items` wraps them,
/// with their arities: the checker wraps each whose result items only a callback gives.
const SETTLED_EMPTY: [(&str, usize); 7] = [
    ("List.map", 2),
    ("List.map_with_index", 2),
    ("List.map2", 3),
    ("List.map3", 4),
    ("List.map4", 5),
    ("List.join_map", 2),
    ("List.map_try", 2),
];

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

/// Declines a call of a local function whose lazy arguments make R calls, unless there is one
/// such argument and the body reads its formal before anything R could observe
/// ([`forced_first`]). R makes the calls where the body first reads the formal, or never, and
/// native code makes them at the call. A typed function an argument calls is taken to make no R
/// call.
unsafe fn lazy_r_calls(
    formals: Sexp,
    body: Sexp,
    values: &[Argument],
    spans: &[ArgumentReads],
    strict: bool,
) -> Result<(), Refusal> {
    let formals = rvalues::pairlist_items(formals);
    let mut calling = values
        .iter()
        .zip(spans)
        .zip(&formals)
        .filter(|((value, span), _)| {
            matches!(value, Argument::Value(_))
                && (span.lazy || !strict)
                && span.first_r_call != span.end_r_call
        })
        .map(|(_, &(symbol, _))| symbol);
    let Some(formal) = calling.next() else {
        return Ok(());
    };
    if calling.next().is_none() && forced_first(body, formal, body) == Some(true) {
        return Ok(());
    }
    Err(Refusal::new(
        "an argument of a local function calls R, which R does where the function first reads \
         the argument: compute the argument into a variable first",
    ))
}

/// Whether evaluating `expression` reads `formal` before anything R could observe: `Some(true)`
/// when the read comes first, `Some(false)` when something else may (any call, an operator or
/// a `Num` function applied before the read, a `return`, an assignment to the formal, or a read
/// on some paths only), and `None` when it neither reads the formal nor does anything. A function `body` defines
/// for itself is a call.
unsafe fn forced_first(expression: Sexp, formal: Sexp, body: Sexp) -> Option<bool> {
    match expression.kind() {
        SexpType::Symbol => return (expression == formal).then_some(true),
        SexpType::Language => {}
        _ => return None,
    }
    let head = expression.car();
    if head.kind() != SexpType::Symbol || assigns(body, head) {
        return Some(false);
    }
    let items = rvalues::pairlist_items(expression.cdr());
    let first = |index: usize| {
        items
            .get(index)
            .and_then(|&(_, item)| forced_first(item, formal, body))
    };
    let in_order = || {
        items
            .iter()
            .find_map(|&(_, item)| forced_first(item, formal, body))
    };
    // An operator or a library call can fail (an `I64` overflow, a division by zero), and its
    // error shows before the read unless one of its operands reads the formal first.
    let applied = || in_order().or(Some(false));
    let name = rvalues::symbol_name(head);
    match &*name {
        "{" | "(" => in_order(),
        "return" => first(0).or(Some(false)),
        "+" | "-" | "*" | "/" | "^" | "%%" | "%/%" | "==" | "!=" | "<" | ">" | "<=" | ">="
        | "!" | "&" | "|" => applied(),
        "&&" | "||" => first(0).or_else(|| first(1).map(|_| false)),
        "if" => first(0).or_else(|| match (first(1), first(2)) {
            (None, None) => None,
            (Some(true), Some(true)) => Some(true),
            _ => Some(false),
        }),
        "<-" | "=" => match items.as_slice() {
            &[(_, target), (_, value)] if target.kind() == SexpType::Symbol && target != formal => {
                forced_first(value, formal, body)
            }
            _ => Some(false),
        },
        _ if name.starts_with("Num.") => applied(),
        _ => Some(false),
    }
}

/// Whether `expression`, local functions included, assigns `symbol` with `<-`, `=`, `<<-`, as a
/// `for` variable or in a `match` pattern.
unsafe fn assigns(expression: Sexp, symbol: Sexp) -> bool {
    if expression.kind() != SexpType::Language {
        return false;
    }
    let head = expression.car();
    let items = rvalues::pairlist_items(expression.cdr());
    // A pattern reads no names, so any name in it is one the arm binds.
    if head.kind() == SexpType::Symbol
        && &*rvalues::symbol_name(head) == "=>"
        && items
            .first()
            .is_some_and(|&(_, pattern)| mentions_outside(pattern, rvalues::nil(), symbol))
    {
        return true;
    }
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

/// Gives the lists `members` hold, and the instructions that allocated them, the item type
/// `item` ([`Lowering::settle`]).
fn retype_family(function: &mut Function, members: &[Local], item: Item) {
    for inst in function
        .blocks
        .iter_mut()
        .flat_map(|block| &mut block.insts)
    {
        if inst.dest.is_some_and(|dest| members.contains(&dest)) {
            match &mut inst.op {
                Op::NewVector(held, _) | Op::EmptyList(held, _) => *held = item,
                Op::Relabel(_, Type::Vector { item: held, .. }) => *held = item,
                _ => {}
            }
        }
    }
    for member in members {
        if let Some(Type::Vector { item: held, .. }) = function.locals.get_mut(member.0 as usize) {
            *held = item;
        }
    }
}

fn hidden_refusal() -> Refusal {
    Refusal::new("a local function defined in a branch or loop, used after it")
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

/// Whether `expression`, the body of a typed function, is small enough to lower into a caller:
/// at most `MAX_INLINE_CALLS` calls, counted in `calls`, and at most `own_calls` calls of `own`,
/// the function's own name.
unsafe fn small_body(
    expression: Sexp,
    own: Sexp,
    own_calls: &mut usize,
    calls: &mut usize,
) -> bool {
    if expression.kind() != SexpType::Language {
        return true;
    }
    *calls += 1;
    let head = expression.car();
    if *calls > MAX_INLINE_CALLS {
        return false;
    }
    if head == own {
        if *own_calls == 0 {
            return false;
        }
        *own_calls -= 1;
    }
    small_body(head, own, own_calls, calls)
        && rvalues::pairlist_items(expression.cdr())
            .into_iter()
            .all(|(_, item)| small_body(item, own, own_calls, calls))
}

/// Whether R returns the value a call of `closure` gives invisibly, as after an assignment, a
/// loop, `invisible()` or `List.for_each`. It is that of the body's last expression and of every
/// `return` in it, following a call in the last position into the local, typed or passed function
/// it calls, as R's visibility flag outlives the callee's return. The error names why compiled
/// code, which does not track the flag per path, cannot show the value as R does.
pub unsafe fn returns_invisibly(closure: Sexp) -> Result<bool, &'static str> {
    let mut walk = Visibilities {
        frames: SmallVec::new(),
    };
    let entry: Reached = smallvec::smallvec![Callee::Typed(closure)];
    match walk.call(&entry, &[], None) {
        None | Some(Shown::Visible) => Ok(false),
        Some(Shown::Invisible) => Ok(true),
        Some(Shown::Mixed) => Err("R returns its value invisibly on some paths only"),
        Some(Shown::Deep) => {
            Err("whether R shows its value depends on calls nested too deep to follow")
        }
    }
}

/// How many function bodies a visibility walk follows calls through.
const MAX_VISIBILITY_DEPTH: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shown {
    Visible,
    Invisible,
    /// Paths that disagree.
    Mixed,
    /// Calls past [`MAX_VISIBILITY_DEPTH`].
    Deep,
}

/// A function a call in a walked body reaches.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Callee {
    /// A `function` literal, with the frame of the body it is written in, where its free names
    /// resolve.
    Literal { literal: Sexp, frame: usize },
    /// A top-level typed function.
    Typed(Sexp),
    /// Any other function, whose value counts as visible.
    Opaque,
}

/// The functions a name may call: a local name can be bound to a literal on several paths.
type Reached = SmallVec<[Callee; 1]>;

struct Visibilities {
    /// The bodies being walked, innermost last.
    frames: SmallVec<[Walked; 4]>,
}

struct Walked {
    formals: Sexp,
    body: Sexp,
    /// The environment the names no enclosing frame binds resolve in.
    env: Sexp,
    /// The frame a local or passed literal is written in.
    parent: Option<usize>,
    /// What the call passed for its formals, for the ones that reach a known function, in the
    /// order of the formals.
    arguments: SmallVec<[(Sexp, Callee); 2]>,
    /// The local functions the body binds by assignment, outside the functions it defines.
    locals: SmallVec<[(Sexp, Sexp); 2]>,
}

fn join(left: Option<Shown>, right: Option<Shown>) -> Option<Shown> {
    match (left, right) {
        (Some(Shown::Deep), _) | (_, Some(Shown::Deep)) => Some(Shown::Deep),
        (Some(left), Some(right)) if left != right => Some(Shown::Mixed),
        (left, right) => left.or(right),
    }
}

impl Visibilities {
    /// What a call of `callees` with `args`, written in frame `caller`, gives. `None` is a call
    /// that gives no value of its own: every path stops, or it calls back into a body being
    /// walked with the same functions, whose value is that of its other paths.
    unsafe fn call(
        &mut self,
        callees: &[Callee],
        args: &[(Sexp, Sexp)],
        caller: Option<usize>,
    ) -> Option<Shown> {
        let mut found = None;
        for &callee in callees {
            let (formals, body, env, parent) = match callee {
                Callee::Opaque => {
                    found = join(found, Some(Shown::Visible));
                    continue;
                }
                Callee::Typed(closure) => (
                    closure.car(),
                    interp::bytecode::closure_body(closure),
                    closure.tag(),
                    None,
                ),
                Callee::Literal { literal, frame } => {
                    let parts = rvalues::pairlist_items(literal.cdr());
                    let (Some(&(_, formals)), Some(&(_, body))) = (parts.first(), parts.get(1))
                    else {
                        found = join(found, Some(Shown::Visible));
                        continue;
                    };
                    (formals, body, self.frames[frame].env, Some(frame))
                }
            };
            let arguments = match caller {
                Some(caller) => self.arguments(formals, args, caller),
                None => SmallVec::new(),
            };
            // A recursion that passes a new literal at each level (`nest(n - 1, \(k) f(k))`)
            // writes it in a new frame each time, so the guard compares literals, not frames.
            let recursive = self.frames.iter().any(|frame| {
                frame.body == body
                    && frame.arguments.len() == arguments.len()
                    && frame
                        .arguments
                        .iter()
                        .zip(&arguments)
                        .all(|(held, passed)| {
                            held.0 == passed.0 && identity(held.1) == identity(passed.1)
                        })
            });
            if recursive {
                continue;
            }
            if self.frames.len() >= MAX_VISIBILITY_DEPTH {
                return Some(Shown::Deep);
            }
            let mut locals = SmallVec::new();
            collect_locals(body, &mut locals);
            self.frames.push(Walked {
                formals,
                body,
                env,
                parent,
                arguments,
                locals,
            });
            let index = self.frames.len() - 1;
            let mut shown = self.tail(body, index);
            self.returns(body, index, &mut shown);
            self.frames.pop();
            found = join(found, shown);
        }
        found
    }

    /// What each formal of a callee reaches from the arguments a call in frame `caller` passes,
    /// matched by exact name, then by position, as R matches them. A function that takes `...`
    /// does not compile, so partial names and arguments after `...` are not modelled.
    unsafe fn arguments(
        &self,
        formals: Sexp,
        args: &[(Sexp, Sexp)],
        caller: usize,
    ) -> SmallVec<[(Sexp, Callee); 2]> {
        let formals = rvalues::pairlist_items(formals);
        let mut passed: SmallVec<[Option<Sexp>; 4]> = smallvec::smallvec![None; formals.len()];
        let mut positional: SmallVec<[Sexp; 4]> = SmallVec::new();
        for &(name, value) in args {
            match formals.iter().position(|&(formal, _)| formal == name) {
                Some(index) if name != rvalues::nil() => passed[index] = Some(value),
                _ => positional.push(value),
            }
        }
        let mut positional = positional.into_iter();
        for slot in passed.iter_mut().filter(|slot| slot.is_none()) {
            *slot = positional.next();
        }
        let mut arguments = SmallVec::new();
        for (&(formal, _), value) in formals.iter().zip(passed) {
            let Some(value) = value else { continue };
            let callees: Reached = if is_function_literal(value) {
                smallvec::smallvec![Callee::Literal {
                    literal: value,
                    frame: caller,
                }]
            } else if value.kind() == SexpType::Symbol {
                self.resolve(value, caller)
            } else {
                continue;
            };
            arguments.extend(
                callees
                    .into_iter()
                    .filter(|&callee| callee != Callee::Opaque)
                    .map(|callee| (formal, callee)),
            );
        }
        arguments
    }

    /// The functions `symbol` names in frame `frame`: a local function or a formal of that frame
    /// or one it is written in, else a top-level typed function.
    unsafe fn resolve(&self, symbol: Sexp, frame: usize) -> Reached {
        let mut current = Some(frame);
        while let Some(index) = current {
            let scope = &self.frames[index];
            let locals: Reached = scope
                .locals
                .iter()
                .filter(|&&(name, _)| name == symbol)
                .map(|&(_, literal)| Callee::Literal {
                    literal,
                    frame: index,
                })
                .collect();
            if !locals.is_empty() {
                return locals;
            }
            if rvalues::pairlist_items(scope.formals)
                .iter()
                .any(|&(name, _)| name == symbol)
            {
                let passed: Reached = scope
                    .arguments
                    .iter()
                    .filter(|&&(formal, _)| formal == symbol)
                    .map(|&(_, callee)| callee)
                    .collect();
                return if passed.is_empty() {
                    smallvec::smallvec![Callee::Opaque]
                } else {
                    passed
                };
            }
            current = scope.parent;
        }
        match find_function(symbol, self.frames[frame].env) {
            Some(speculation)
                if speculation.function.kind() == SexpType::Closure
                    && interp::annotations::is_typed(speculation.function) =>
            {
                smallvec::smallvec![Callee::Typed(speculation.function)]
            }
            _ => smallvec::smallvec![Callee::Opaque],
        }
    }

    unsafe fn tail(&mut self, expression: Sexp, frame: usize) -> Option<Shown> {
        let visible = Some(Shown::Visible);
        if expression.kind() != SexpType::Language || expression.car().kind() != SexpType::Symbol {
            return visible;
        }
        let head = expression.car();
        let args = rvalues::pairlist_items(expression.cdr());
        let argument = |index: usize| args.get(index).map(|&(_, value)| value);
        match &*rvalues::symbol_name(head) {
            // A loop that only a `return` leaves gives no value of its own.
            "repeat" if argument(0).is_some_and(|body| !breaks(body)) => None,
            "while"
                if argument(0).is_some_and(|condition| is_true(condition))
                    && argument(1).is_some_and(|body| !breaks(body)) =>
            {
                None
            }
            "<-" | "=" | "<<-" | "invisible" | "for" | "while" | "repeat" | "List.for_each" => {
                Some(Shown::Invisible)
            }
            "{" => match args.last() {
                Some(&(_, last)) => self.tail(last, frame),
                None => visible,
            },
            "if" => {
                let then = match argument(1) {
                    Some(then) => self.tail(then, frame),
                    None => visible,
                };
                // An `if` without `else` whose condition is false gives an invisible `NULL`.
                let otherwise = match argument(2) {
                    Some(otherwise) => self.tail(otherwise, frame),
                    None => Some(Shown::Invisible),
                };
                join(then, otherwise)
            }
            ".ry_match" => args
                .iter()
                .skip(1)
                .filter(|&&(_, arm)| arm.kind() == SexpType::Language)
                .filter_map(|&(_, arm)| rvalues::pairlist_items(arm.cdr()).last().copied())
                .fold(None, |found, (_, body)| join(found, self.tail(body, frame))),
            "return" => match argument(0) {
                // GNU R shows `return(y <- x)` and the interpreter does not, so the interpreter
                // runs it rather than native code picking one.
                Some(value)
                    if value.kind() == SexpType::Language
                        && value.car().kind() == SexpType::Symbol
                        && matches!(&*rvalues::symbol_name(value.car()), "<-" | "=" | "<<-") =>
                {
                    Some(Shown::Mixed)
                }
                Some(value) => self.tail(value, frame),
                None => visible,
            },
            // A path that stops or leaves a loop gives no value.
            "stop" | "break" | "next" => None,
            _ => {
                let callees = self.resolve(head, frame);
                self.call(&callees, &args, Some(frame))
            }
        }
    }

    /// Joins into `found` what each `return` in `expression` gives, outside the functions it
    /// defines. The error a `?` returns early is visible.
    unsafe fn returns(&mut self, expression: Sexp, frame: usize, found: &mut Option<Shown>) {
        if expression.kind() != SexpType::Language {
            return;
        }
        let head = expression.car();
        if head.kind() == SexpType::Symbol {
            match &*rvalues::symbol_name(head) {
                "function" => return,
                "return" => *found = join(*found, self.tail(expression, frame)),
                ".ry_try" => *found = join(*found, Some(Shown::Visible)),
                _ => {}
            }
        }
        let mut items = expression;
        while items.kind() == SexpType::Language || items.kind() == SexpType::Pairlist {
            self.returns(items.car(), frame, found);
            items = items.cdr();
        }
    }
}

/// Records each local function `expression` binds by assignment, outside the functions it
/// defines.
unsafe fn collect_locals(expression: Sexp, locals: &mut SmallVec<[(Sexp, Sexp); 2]>) {
    if expression.kind() != SexpType::Language {
        return;
    }
    let head = expression.car();
    if head.kind() == SexpType::Symbol {
        let name = rvalues::symbol_name(head);
        if name == "function" {
            return;
        }
        if name == "<-" || name == "=" {
            let parts = rvalues::pairlist_items(expression.cdr());
            if let (Some(&(_, target)), Some(&(_, value))) = (parts.first(), parts.get(1))
                && target.kind() == SexpType::Symbol
                && is_function_literal(value)
            {
                locals.push((target, value));
                return;
            }
        }
    }
    let mut items = expression;
    while items.kind() == SexpType::Language || items.kind() == SexpType::Pairlist {
        collect_locals(items.car(), locals);
        items = items.cdr();
    }
}

/// Whether a `break` in `expression` leaves the loop it is the body of: one outside the loops and
/// functions it holds.
unsafe fn breaks(expression: Sexp) -> bool {
    if expression.kind() == SexpType::Symbol {
        return rvalues::symbol_name(expression) == "break";
    }
    if expression.kind() != SexpType::Language {
        return false;
    }
    let head = expression.car();
    if head.kind() == SexpType::Symbol {
        match &*rvalues::symbol_name(head) {
            "break" => return true,
            "function" | "for" | "while" | "repeat" => return false,
            _ => {}
        }
    }
    let mut items = expression;
    while items.kind() == SexpType::Language || items.kind() == SexpType::Pairlist {
        if breaks(items.car()) {
            return true;
        }
        items = items.cdr();
    }
    false
}

/// The function a reached callee is, whatever frame its literal is written in.
fn identity(callee: Callee) -> Option<Sexp> {
    match callee {
        Callee::Literal { literal, .. } => Some(literal),
        Callee::Typed(closure) => Some(closure),
        Callee::Opaque => None,
    }
}

unsafe fn is_true(value: Sexp) -> bool {
    value.kind() == SexpType::Logical && rvalues::length(value) == 1 && rvalues::ints(value)[0] == 1
}

unsafe fn is_function_literal(value: Sexp) -> bool {
    value.kind() == SexpType::Language
        && value.car().kind() == SexpType::Symbol
        && rvalues::symbol_name(value.car()) == "function"
}

/// Whether `expression` mentions `symbol` anywhere but inside `skipped`.
unsafe fn mentions_outside(expression: Sexp, skipped: Sexp, symbol: Sexp) -> bool {
    if expression == skipped {
        return false;
    }
    match expression.kind() {
        SexpType::Symbol => expression == symbol,
        SexpType::Language => {
            mentions_outside(expression.car(), skipped, symbol)
                || rvalues::pairlist_items(expression.cdr())
                    .into_iter()
                    .any(|(_, item)| mentions_outside(item, skipped, symbol))
        }
        _ => false,
    }
}

/// The names any of `ends` created, in the order they first appear.
fn created_symbols(ends: &[(BlockId, Created)]) -> SmallVec<[Sexp; 8]> {
    let mut symbols: SmallVec<[Sexp; 8]> = SmallVec::new();
    for (_, created) in ends {
        for &(symbol, _, _) in created {
            if !symbols.contains(&symbol) {
                symbols.push(symbol);
            }
        }
    }
    symbols
}

/// The variable each of `ends` left `symbol` in, and whether it may be unset there.
fn created_in(ends: &[(BlockId, Created)], symbol: Sexp) -> SmallVec<[Option<(Local, bool)>; 4]> {
    ends.iter()
        .map(|(_, created)| {
            created
                .iter()
                .find(|&&(other, _, _)| other == symbol)
                .map(|&(_, local, maybe)| (local, maybe))
        })
        .collect()
}

/// Collects the `if` and `match` calls whose value is the value of `expression`: `expression`
/// itself, the last expression of a block, the branches of an `if` and the bodies of a `match`'s
/// arms.
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
            tails.push(expression);
            for &(_, branch) in args.iter().skip(1) {
                tail_matches(branch, tails);
            }
        }
        ".ry_match" => {
            tails.push(expression);
            for &(_, arm) in args.iter().skip(1) {
                if arm.kind() == SexpType::Language
                    && let Some(&(_, body)) = rvalues::pairlist_items(arm.cdr()).last()
                {
                    tail_matches(body, tails);
                }
            }
        }
        _ => {}
    }
}

/// The type a `match` over any tag lowers its arms against when the arms disagree only because
/// R confirmed a stray shape: one with the layout of another candidate (`:One(F64)` beside the
/// `:One(I64)` a function declares). The arms of shapes whose layout no other candidate has
/// must all join, and decide the target; with none, the type most arms give does. `None` when
/// the arms agree or no stray shape explains why they do not, as when a tag has no arm that
/// gives the target.
fn stray_target(cases: &[Option<u32>], types: &[Type]) -> Option<Type> {
    let layouts: SmallVec<[Option<u32>; 4]> = cases
        .iter()
        .map(|case| case.map(|shape| shapes::shape(shape).layout))
        .collect();
    let shared = |position: usize| {
        layouts[position].is_some_and(|layout| {
            layouts
                .iter()
                .enumerate()
                .any(|(other, &found)| other != position && found == Some(layout))
        })
    };
    if !(0..cases.len()).any(shared) {
        return None;
    }
    let mut all = types.iter().copied();
    let first = all.next()?;
    if all.try_fold(first, joined).is_some() {
        return None;
    }
    let mut unique = (0..cases.len()).filter(|&position| !shared(position));
    let reaches = |target: Type| {
        types
            .iter()
            .filter(|&&ty| joined(target, ty).is_some())
            .count()
    };
    let target = match unique.next() {
        Some(position) => unique.try_fold(types[position], |held, position| {
            joined(held, types[position])
        })?,
        None => types
            .iter()
            .copied()
            .fold(None, |best: Option<Type>, ty| match best {
                Some(best) if reaches(best) >= reaches(ty) => Some(best),
                _ => Some(ty),
            })?,
    };
    // Each tag keeps a shape whose arm gives the target, or the arms disagree for a reason no
    // stray shape explains (a statement whose arms give different types).
    let kept = (0..cases.len())
        .filter(|&position| shared(position))
        .all(|position| {
            (0..cases.len()).any(|other| {
                layouts[other] == layouts[position] && joined(target, types[other]).is_some()
            })
        });
    kept.then_some(target)
}

/// The refusal of a call of the R function `head` whose value typed code uses directly.
unsafe fn r_value_refusal(head: Sexp) -> Refusal {
    Refusal(Cow::Owned(format!(
        "it uses the value of R's `{}` directly, and typed code calls R only as a statement or \
         inside `Num.from_r`, `Str.from_r`, `Bool.from_r`, `List.from_r`, `List.from_r_or` or \
         `List.from_r_missing`, which give the value a ry type",
        interp::deparse::deparse_first_line(head)
    )))
}

/// What compiled code does with the value of an R call used as `r_use` says.
unsafe fn r_result(r_use: RUse) -> Result<RResult, Refusal> {
    Ok(match r_use {
        RUse::Unused | RUse::Diverges => RResult::Unused,
        RUse::Converted(conversion, _) => {
            RResult::Converted(conversion, conversion_type(conversion)?)
        }
        RUse::Table => RResult::Table,
    })
}

/// The type a conversion of what an R call returns gives: fixed for `Str.from_r` and
/// `Bool.from_r`, and for `Num.from_r` and `List.from_r` the type the conversion gave when the
/// interpreter ran it, which the code guesses it gives again ([`Conversion::guessed`]). A
/// conversion that has given two types has no type to guess, so the code does not compile. A
/// conversion the checker typed takes that type, which it gives or fails with when the code runs
/// (`interp::arrays::checked_conversion` and `interp::numbers::checked_number`).
/// `List.from_r_or` types as `List.from_r`, and `List.from_r_missing` gives a list of
/// `:Ok(item)` or `:Err(:Missing)` for the item type `List.from_r` would give.
unsafe fn conversion_type(conversion: Conversion) -> Result<Type, Refusal> {
    let name = rvalues::symbol_name(conversion.call.car());
    let ty = items_type(conversion, &name)?;
    if name != "List.from_r_missing" {
        return Ok(ty);
    }
    let Type::Vector { item, .. } = ty else {
        return Err(Refusal::new(
            "a `List.from_r_missing` of an unexpected type",
        ));
    };
    let ok = shapes::intern(tag_layout("Ok", 1)?, &[item])
        .ok_or(Refusal::new("a value with more fields than a box holds"))?;
    let missing = shapes::intern(tag_layout("Err", 1)?, &[Item::Tag(shapes::ANY)])
        .ok_or(Refusal::new("a value with more fields than a box holds"))?;
    Ok(Type::Vector {
        item: Item::Tag(shapes::union_of(&[ok, missing])),
        array: true,
    })
}

/// [`conversion_type`] before `List.from_r_missing` wraps each item in a result.
unsafe fn items_type(conversion: Conversion, name: &str) -> Result<Type, Refusal> {
    let awaiting = || {
        AWAITED.set(Some(Awaited::Conversion(conversion.call)));
        Refusal(Cow::Owned(format!(
            "{AWAITING}{}` once, which gives the type the conversion returns",
            interp::deparse::deparse_first_line(conversion.call)
        )))
    };
    let list = |item| Type::Vector { item, array: true };
    let observed = match name {
        "Str.from_r" => return Ok(Type::Str),
        "Bool.from_r" => return Ok(Type::Lgl),
        "Num.from_r" if conversion.checked.is_some() => {
            return match conversion.checked {
                Some(Item::I64) => Ok(Type::I64),
                _ => Ok(Type::Dbl),
            };
        }
        // The conversion gives the checker's type or fails, whatever the interpreter saw it give.
        _ if conversion.checked.is_some() => {
            return conversion.checked.map(list).ok_or_else(awaiting);
        }
        _ => match interp::numbers::observed_conversion(conversion.call) {
            Some(Observed::Steady(kind)) => kind,
            Some(Observed::Changed(before, after)) => {
                let named = |kind| observed_name(name, kind);
                return Err(Refusal(Cow::Owned(format!(
                    "`{}` gave {} in one call and {} in another, so its type is not known before \
                     it runs: convert the value to one type in R first",
                    interp::deparse::deparse_first_line(conversion.call),
                    named(before),
                    named(after)
                ))));
            }
            None => return Err(awaiting()),
        },
    };
    match (name, observed) {
        ("Num.from_r", SexpType::Real) => Ok(Type::Dbl),
        ("Num.from_r", SexpType::I64) => Ok(Type::I64),
        ("Num.from_r", _) => Err(Refusal::new("a `Num.from_r` of an unexpected type")),
        (_, SexpType::Real) => Ok(list(Item::Dbl)),
        (_, SexpType::Logical) => Ok(list(Item::Lgl)),
        (_, SexpType::String) => Ok(list(Item::Str)),
        (_, SexpType::I64) => Ok(list(Item::I64)),
        _ => Err(Refusal::new(
            "a `List.from_r` of an R list, whose items typed code cannot take from R yet",
        )),
    }
}

/// The ry type, in backticks, of the value the conversion `name` gave when the value's R type
/// was `kind`.
pub fn observed_name(name: &str, kind: SexpType) -> String {
    let item = match kind {
        SexpType::Real => "F64",
        SexpType::I64 => "I64",
        SexpType::Logical => "Bool",
        SexpType::String => "Str",
        _ => kind.name(),
    };
    if name == "List.from_r_missing" {
        format!("`List<Result<{item}, :Missing>>`")
    } else if name.starts_with("List.") {
        format!("`List<{item}>`")
    } else {
        format!("`{item}`")
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
    // A lazy-loaded namespace binds each function to a promise that reads it from the package's
    // database. Reading it changes nothing a program can observe, so the compiler forces it. Any
    // other promise runs code, which is the interpreter's to do when the code runs. So is a
    // read that fails: the interpreter forces the promise again and the program gets the error.
    if let Some(cell) = interp::heap().frame_cell(namespace, symbol)
        && cell.cdr().kind() == SexpType::Promise
        && cell.cdr().car() == rvalues::unbound()
    {
        let loaded = match interp::methods_core::is_lazy_load_promise(cell.cdr()) {
            Ok(true) => interp::eval::force_promise(cell.cdr()).is_ok(),
            Ok(false) => false,
            Err(_) => {
                return Some(Err(Refusal::new(
                    "the session cannot name `lazyLoadDBfetch`, so it cannot tell a lazy-loaded \
                     function from a promise that runs code",
                )));
            }
        };
        if !loaded {
            AWAITED.set(Some(Awaited::Promise(cell.cdr())));
            return Some(Err(Refusal(Cow::Owned(format!(
                "{AWAITING}{}` once, which loads the function",
                interp::deparse::deparse_first_line(head)
            )))));
        }
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

/// Whether `name` is an operator the table API's expressions define.
fn is_operator(name: &str) -> bool {
    matches!(
        name,
        "+" | "-"
            | "*"
            | "/"
            | "^"
            | "%%"
            | "%/%"
            | "=="
            | "!="
            | "<"
            | "<="
            | ">"
            | ">="
            | "!"
            | "&"
            | "|"
    )
}

/// Whether `name` is one of the language's own forms rather than a function.
fn is_language_keyword(name: &str) -> bool {
    matches!(
        name,
        "function" | "if" | "for" | "while" | "repeat" | "return" | "break" | "next"
    )
}

/// Whether a base function's arguments are those of an R call, as the checker reads them: it is
/// written as a call, `f(x)`, or as a `%op%` operator. An operand of another operator, of `$` or
/// of `[[` is a ry value.
fn takes_r_arguments(name: &str) -> bool {
    let syntactic = name.starts_with(|first: char| first.is_alphabetic() || first == '.')
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '.' | '_'));
    let special = name.len() > 2
        && name.starts_with('%')
        && name.ends_with('%')
        && !matches!(name, "%%" | "%/%");
    syntactic || special
}

/// Whether a call resolved to one of the table API's functions that gives a table value: the
/// `ry` namespace's own function of that name, which `package:ry` binds too.
pub(crate) unsafe fn is_table_function(speculation: &Speculation) -> bool {
    if !gives_table_value(&rvalues::symbol_name(speculation.symbol)) {
        return false;
    }
    let Ok(package) = rvalues::install("ry") else {
        return false;
    };
    let heap = interp::heap();
    loaded_namespace(package)
        .and_then(|namespace| heap.frame_cell(namespace, speculation.symbol))
        .is_some_and(|cell| cell.cdr() == speculation.function)
}

/// Whether the API function `name` gives one of the API's own values. Every function of the
/// `Expr`, `DataFrame`, `LazyFrame`, `GroupBy` and `LazyGroupBy` modules does, except the ones that
/// read a table out into R values, write it somewhere or change it in place. A function that gives
/// a table value changes nothing a second run of it would see.
fn gives_table_value(name: &str) -> bool {
    const R_VALUES: [&str; 9] = [
        "DataFrame.height",
        "DataFrame.width",
        "DataFrame.columns",
        "DataFrame.schema",
        "DataFrame.key",
        "DataFrame.set_key",
        "DataFrame.write_csv",
        "DataFrame.write_parquet",
        "LazyFrame.explain",
    ];
    if matches!(name, "DataFrame" | "as.DataFrame") {
        return true;
    }
    let Some((module, _)) = name.split_once('.') else {
        return false;
    };
    matches!(
        module,
        "Expr" | "DataFrame" | "LazyFrame" | "GroupBy" | "LazyGroupBy"
    ) && !R_VALUES.contains(&name)
}

/// Whether `function` is a typed closure that declares a table value as its result.
pub(crate) unsafe fn returns_table(function: Sexp) -> bool {
    function.kind() == SexpType::Closure
        && interp::annotations::return_annotation(function).is_some_and(|declared| {
            let name = declared
                .trim()
                .split(|next: char| !next.is_alphanumeric())
                .next()
                .unwrap_or_default();
            matches!(
                name,
                "Expr" | "DataFrame" | "LazyFrame" | "GroupBy" | "LazyGroupBy"
            )
        })
}

pub(crate) unsafe fn is_base_function(speculation: &Speculation) -> bool {
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

/// What a function lookup of `symbol` from `start` finds for code being lowered: the binding,
/// or, while a script's definitions compile before the script runs, the closure the script
/// will bind the name to. Such a lookup has no binding to record yet.
pub unsafe fn find_function(symbol: Sexp, start: Sexp) -> Option<Speculation> {
    match crate::ahead::defined_later(symbol, start) {
        Some(function) => Some(Speculation {
            symbol,
            env: start,
            cell: rvalues::nil(),
            function,
        }),
        None => resolve_function(symbol, start),
    }
}

/// Walks from `start` to the binding a function lookup of `symbol` finds, as the interpreter
/// does, but gives up instead of calling an active binding or forcing a promise that may run
/// arbitrary code.
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
                    // An installed package binds its closures to lazy-load promises, which only
                    // read the function from the package database, so forcing one is safe. Any
                    // other promise but a lazy definition's may run arbitrary code. A load that
                    // fails leaves the call to the interpreter, which raises the error itself.
                    let reads_database = env == heap.known().base_env
                        || env == heap.known().base_namespace
                        || matches!(interp::serialize::lazy_record_header(value), Ok(Some(_)));
                    // `ry load` binds a definition to a promise that lowers its code and
                    // evaluates it. Only a function definition and a pure constant of a
                    // strict file are bound that way, so forcing one early changes nothing a
                    // program can observe.
                    let code = value.cdr();
                    let lazy_definition = code.kind() == SexpType::Language
                        && rvalues::install("ryLoaderForce")
                            .ok()
                            .and_then(|symbol| interp::builtins::internal_function(symbol))
                            == Some(code.car());
                    if !reads_database && !lazy_definition {
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

/// What a read of a variable from outside a typed function finds.
pub enum Global {
    /// The value of a binding in the global environment, a namespace or a package on the
    /// search path.
    Value(Sexp),
    Unbound,
    /// A binding in the frame of an enclosing function.
    Enclosing,
    /// A promise not yet forced, such as a lazy-load promise of a package's variable.
    Promise(Sexp),
    /// An active binding, whose read calls R.
    Active,
}

/// What a read of `symbol` from `start` finds, outside the typed function whose environment
/// `start` is.
pub unsafe fn global_binding(symbol: Sexp, start: Sexp) -> Global {
    let heap = interp::heap();
    let known = heap.known();
    let mut env = start;
    // A namespace's parent is its imports environment, which binds what the package imports.
    let mut imports = false;
    while env != known.empty_env {
        let namespace = interp::envir::is_namespace(env);
        if let Some(cell) = heap.frame_cell(env, symbol) {
            if env != known.global_env && !namespace && !imports && !on_search_path(env) {
                return Global::Enclosing;
            }
            if cell.has_gp(heap::object::gp::ACTIVE_BINDING) {
                return Global::Active;
            }
            let value = cell.cdr();
            if value.kind() != SexpType::Promise {
                return Global::Value(value);
            }
            if value.car() == rvalues::unbound() {
                return Global::Promise(value);
            }
            return Global::Value(value.car());
        }
        imports = namespace;
        env = env.enclosure();
    }
    Global::Unbound
}

/// Whether `env` is the global environment or an environment after it on the search path.
unsafe fn on_search_path(env: Sexp) -> bool {
    let known = interp::heap().known();
    let mut search = known.global_env;
    while search != known.empty_env {
        if search == env {
            return true;
        }
        search = search.enclosure();
    }
    false
}

/// Whether native code reads a global of type `ty`, the types a variable from R takes that are
/// ry values: a number, a `Bool`, a `Str`, a list of them, or a table value.
pub fn reads_globally(ty: Type) -> bool {
    match ty {
        Type::Dbl | Type::I64 | Type::Lgl | Type::Str | Type::Table => true,
        Type::Vector { item, array } => {
            array && matches!(item, Item::Dbl | Item::I64 | Item::Lgl | Item::Str)
        }
        _ => false,
    }
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
