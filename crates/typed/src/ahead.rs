//! Typed closures compiled before the script that defines them runs (`ry run --aot`).
//!
//! [`compile`] is handed the closures of a script's top-level definitions before the script's
//! first expression runs. It compiles each typed one for the argument types its annotations
//! declare, which are the types [`crate::call`] reads from values of those types, and with it
//! every typed function it calls. A call the script then makes finds the code in the table a
//! first call would have filled, so it compiles nothing.
//!
//! No definition has run when the functions compile, so the names they call are not bound yet.
//! A call of a function the script defines resolves to the closure made for that definition
//! ([`defined_later`]), and the lookup it records holds no binding. Every call of compiled code
//! checks its lookups again, so code compiled against a definition that never ran, or whose
//! name was bound to something else since, is not used: the function compiles again for what
//! the name finds then, as any typed function does after a rebinding.
//!
//! A function compiles ahead only when each parameter declares a type that says which values
//! arrive from R. One that takes a record or a function compiles when a typed caller compiles,
//! for the types the caller passes.

use {
    crate::{
        AWAITING, COMPILED, Refusal, compiled_or_refusal, declared_type,
        ir::{Item, Type},
        shapes,
    },
    heap::object::Sexp,
    interp::{ahead::Program, annotations, values as rvalues},
    rustc_hash::FxHashMap,
    smallvec::SmallVec,
    std::{
        borrow::Cow,
        cell::{Cell, RefCell},
        time::Instant,
    },
};

thread_local! {
    /// The closures of the script's definitions by the name each will be bound to, while
    /// [`compile`] runs.
    static DEFINED_LATER: RefCell<FxHashMap<Sexp, Sexp>> = RefCell::new(FxHashMap::default());
    /// Whether [`DEFINED_LATER`] holds anything, which every function lookup asks first.
    static COMPILING_AHEAD: Cell<bool> = const { Cell::new(false) };
    /// Whether a script's definitions were compiled ahead, so that a compilation after it is
    /// one the run pays for.
    static COMPILED_AHEAD: Cell<bool> = const { Cell::new(false) };
}

/// How many rounds [`compile`] compiles its functions again because a later one confirmed a
/// shape that a `match` in an earlier one must test for.
const MAX_ROUNDS: usize = 4;

/// How many names of types an annotation is followed through before it counts as unknown, which
/// ends a definition that names itself.
const MAX_NAMED_DEPTH: usize = 8;

/// Compiles the typed closures among the definitions of `program` for the argument types they
/// declare.
pub unsafe fn compile(program: &Program<'_>) {
    let started = Instant::now();
    let definitions = program.definitions;
    DEFINED_LATER.with_borrow_mut(|later| {
        later.clear();
        // A name the script defines more than once stands for its first definition, the one
        // every call before the second finds. Which one is picked decides only how much of the
        // code compiled here the run uses, because every call checks its lookups again.
        for definition in definitions {
            later.entry(definition.symbol).or_insert(definition.closure);
        }
    });
    COMPILING_AHEAD.set(true);
    let logging = annotations::typed_logging();
    let mut roots: Vec<(Sexp, SmallVec<[Type; 4]>)> = Vec::new();
    for definition in definitions {
        if !annotations::is_typed(definition.closure) {
            continue;
        }
        let name = || rvalues::symbol_name(definition.symbol);
        match declared_params(definition.closure, program.named_types) {
            Ok(params) => match compiled_or_refusal(definition.closure, &params) {
                Ok(_) => roots.push((definition.closure, params)),
                Err(refusal) => {
                    if logging {
                        eprintln!(
                            "typed: `{}` is not compiled ahead of time for {params:?}: {}",
                            name(),
                            refusal.0
                        );
                    }
                }
            },
            Err(refusal) => {
                if logging {
                    eprintln!(
                        "typed: `{}` is not compiled ahead of time on its own: {}",
                        name(),
                        refusal.0
                    );
                }
            }
        }
    }
    // A `match` over any tag tests for the shapes confirmed when it compiled, and a function
    // compiled later may have confirmed more. Asking for the code again compiles what is stale,
    // until a round confirms nothing new.
    for _ in 0..MAX_ROUNDS {
        let epoch = shapes::epoch();
        for (closure, params) in &roots {
            // A refusal here leaves the function to compile at its first call.
            if compiled_or_refusal(*closure, params).is_err() && logging {
                eprintln!("typed: a function compiled ahead of time no longer compiles");
            }
        }
        if shapes::epoch() == epoch {
            break;
        }
    }
    COMPILING_AHEAD.set(false);
    DEFINED_LATER.with_borrow_mut(|later| later.clear());
    // A refusal met here may only say that the script had not run yet, as for a call of a
    // function the script builds at run time, so the first call tries again.
    COMPILED.with_borrow_mut(|table| table.retain(|_, entry| entry.is_ok()));
    AWAITING.with_borrow_mut(|awaiting| awaiting.clear());
    COMPILED_AHEAD.set(true);
    if logging {
        let functions = COMPILED.with_borrow(|table| table.len());
        eprintln!(
            "typed: compiled {functions} function{} ahead of time in {} us",
            if functions == 1 { "" } else { "s" },
            started.elapsed().as_micros()
        );
    }
}

/// Whether a script's definitions were compiled before it ran, which makes every later
/// compilation one that happens during the run.
pub fn compiled_ahead() -> bool {
    COMPILED_AHEAD.get()
}

/// Whether [`compile`] is running, so that function lookups go through [`defined_later`].
#[inline]
pub(crate) fn compiling_ahead() -> bool {
    COMPILING_AHEAD.get()
}

/// The closure a function lookup of `symbol` from `start` will find once the script has run the
/// first definition of that name, when the lookup starts in the global environment and no
/// binding there shadows it yet.
pub(crate) unsafe fn defined_later(symbol: Sexp, start: Sexp) -> Option<Sexp> {
    if !COMPILING_AHEAD.get() || start != rvalues::global_env() {
        return None;
    }
    if interp::heap().frame_cell(start, symbol).is_some() {
        return None;
    }
    DEFINED_LATER.with_borrow(|later| later.get(&symbol).copied())
}

/// The argument types a typed closure declares, as [`crate::call`] reads them from values of
/// the declared types.
unsafe fn declared_params(
    closure: Sexp,
    named_types: &FxHashMap<String, String>,
) -> Result<SmallVec<[Type; 4]>, Refusal> {
    let formals = closure.car();
    let body = interp::bytecode::closure_body(closure);
    let (declared, _) = annotations::declared_formals(formals, body);
    let mut params = SmallVec::new();
    for (position, (symbol, _)) in rvalues::pairlist_items(formals).into_iter().enumerate() {
        if symbol == rvalues::symbols().dots {
            return Err(Refusal::new("it takes `...`"));
        }
        let ty = if annotations::declares_i64(closure, position) {
            Some(Type::I64)
        } else {
            annotations::formal_annotation(formals, body, position)
                .and_then(|annotation| annotated_type(&annotation, named_types, 0))
                .or_else(|| {
                    declared
                        .get(position)
                        .copied()
                        .flatten()
                        .and_then(declared_type)
                })
        };
        params.push(ty.ok_or_else(|| {
            Refusal(Cow::Owned(format!(
                "the type of parameter `{}` does not say which values arrive from R, so it \
                 compiles with the typed function that calls it, or at its first call",
                rvalues::symbol_name(symbol)
            )))
        })?);
    }
    Ok(params)
}

/// The type of the values an annotation's text admits from R, for the annotations whose type
/// the checks' storage masks do not give. A tag from R is any tag, whichever union the
/// annotation names, because R code can build every tag. A record from R has no type of its own
/// and a set or a dictionary has no R value, so a parameter of one is never passed from R.
fn annotated_type(
    annotation: &str,
    named_types: &FxHashMap<String, String>,
    depth: usize,
) -> Option<Type> {
    let annotation = annotation.trim();
    if let Some(definition) = named_types.get(annotation) {
        return (depth < MAX_NAMED_DEPTH)
            .then(|| annotated_type(definition, named_types, depth + 1))
            .flatten();
    }
    if annotation.starts_with(':') {
        return Some(Type::Tag(shapes::ANY));
    }
    if let Some(items) = annotation.strip_suffix("[]") {
        return annotated_item(items, named_types, depth).map(Type::vector);
    }
    if let Some(items) = annotation
        .strip_prefix("List")
        .and_then(|rest| rest.trim_start().strip_prefix('<'))
        .and_then(|rest| rest.strip_suffix('>'))
    {
        return annotated_item(items, named_types, depth)
            .map(|item| Type::Vector { item, array: true });
    }
    if matches!(
        annotation,
        "DataFrame" | "LazyFrame" | "GroupBy" | "LazyGroupBy" | "Expr"
    ) {
        return Some(Type::Table);
    }
    annotated_item(annotation, named_types, depth).map(Type::of_item)
}

fn annotated_item(
    name: &str,
    named_types: &FxHashMap<String, String>,
    depth: usize,
) -> Option<Item> {
    let name = name.trim();
    if let Some(definition) = named_types.get(name) {
        return (depth < MAX_NAMED_DEPTH)
            .then(|| annotated_item(definition, named_types, depth + 1))
            .flatten();
    }
    Some(match name {
        "I64" => Item::I64,
        "F64" | "double" => Item::Dbl,
        "integer" => Item::Int,
        "Bool" | "logical" => Item::Lgl,
        "Str" | "character" => Item::Str,
        _ => return None,
    })
}
