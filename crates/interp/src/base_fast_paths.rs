//! Native fast paths for hot base R closures. Validation code calls `match.arg`, `stopifnot`,
//! `structure`, `setdiff` and `format()` at every API boundary, and their R bodies spend most of
//! their time in a dozen nested closure calls (`sys.function`, `formals`, `eval`, `identical`,
//! `tryCatch`) that each cost a full call. When base loads, [`install_all`] puts a one-call prologue
//! in front of each body, `.Internal(ryMatchArg())` and so on. The prologue either finishes the
//! call with the result and the effects the R body would have, by returning from the closure's
//! frame as `return()` does, or changes nothing observable and lets the R body run. A prologue
//! forces only arguments the R body forces, in the same order, and a forced promise keeps its
//! value, so the R body that runs after a declined fast path sees the same arguments.
//!
//! Each fast path follows the code of one GNU R release, so it is installed only when the loaded
//! function is exactly that code, recognised by its hash; any other version keeps its own body.

use {
    crate::{
        Jump,
        builtins::{Arg, BuiltinDef, BuiltinFn, Call, internal},
        eval, interp,
        session::{FNV_OFFSET, stable_hash},
        sort::{self, IdenticalFlags},
        unwind::{R, Unwind, jump},
        values::{self, install, length, missing_arg, nil, symbols},
    },
    heap::{
        activity::{self, Counter},
        object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType, gp},
    },
    smallvec::SmallVec,
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("ryMatchArg", 0, do_match_arg),
    internal("ryStopifnot", 0, do_stopifnot),
    internal("rySetdiff", 0, do_setdiff),
    internal("ryIntersect", 0, do_intersect),
    internal("ryStructure", 0, do_structure),
    internal("rySysGetenv", 0, do_sys_getenv),
    internal("rySysSetenv", 0, do_sys_setenv),
    internal("ryFormatDefault", 0, do_format_default),
    internal("ryReduce", 0, do_reduce),
    internal("ryAssignNativeRoutines", 0, do_assign_native_routines),
];

/// The base functions that get a fast path, with the internal their prologue calls and the
/// [`stable_hash`] of the formals and body each was made from (GNU R 4.6.2). Setting
/// `RY_PRINT_FAST_PATH_HASHES` prints the hashes of the loaded ones.
const FAST_PATHS: &[(&str, &str, u64)] = &[
    ("match.arg", "ryMatchArg", 0x0d569925082b596e),
    ("stopifnot", "ryStopifnot", 0x036d82929ad9613c),
    ("setdiff", "rySetdiff", 0x9f92f07db3152727),
    ("intersect", "ryIntersect", 0xa759f096d9e1805d),
    ("structure", "ryStructure", 0xe1de236049200229),
    ("Sys.getenv", "rySysGetenv", 0xe91342790fcd5c16),
    ("Sys.setenv", "rySysSetenv", 0xb3ad8ec3b2915ce0),
    ("format.default", "ryFormatDefault", 0x5eb64c51109fbc94),
    ("Reduce", "ryReduce", 0xd929e4c219983788),
];

/// The closures that a base function defines in its own body and that get a fast path: the
/// outer function, the inner closure's name, the internal its prologue calls and the
/// [`stable_hash`] of the inner closure's formals and body (GNU R 4.6.2).
const NESTED_FAST_PATHS: &[(&str, &str, &str, u64)] = &[(
    "loadNamespace",
    "assignNativeRoutines",
    "ryAssignNativeRoutines",
    0x26c985af62edd0db,
)];

/// Puts the fast paths in front of the base namespace's functions, changing each closure in
/// place so that every reference to it gets the fast path. ry builds its bundled base R itself,
/// so the namespace it saved already holds the prologues; on that version every fast path must
/// install, so a mismatch is an error rather than a silent loss of speed.
pub unsafe fn install_all(namespace: Sexp) -> R<()> {
    let print = std::env::var_os("RY_PRINT_FAST_PATH_HASHES").is_some();
    let bundled =
        crate::system::r_version().number() == crate::methods_core::BUNDLED_VERSION.trim();
    let mut missed = Vec::new();
    for &(name, internal_name, expected) in FAST_PATHS {
        let function = eval::value_of(eval::find_var_in_frame(install(name)?, namespace)?)?;
        if !install_prologue(function, name, internal_name, expected, print)? {
            missed.push(name);
        }
    }
    for &(outer, inner, internal_name, expected) in NESTED_FAST_PATHS {
        let function = eval::value_of(eval::find_var_in_frame(install(outer)?, namespace)?)?;
        if !install_nested_prologue(function, inner, internal_name, expected, print)? {
            missed.push(inner);
        }
    }
    if bundled && !missed.is_empty() {
        return Err(crate::unwind::error(format!(
            "the native fast paths for {} do not match ry's bundled base R; update FAST_PATHS in \
             base_fast_paths.rs (RY_PRINT_FAST_PATH_HASHES=1 prints the hashes)",
            missed.join(", ")
        )));
    }
    register_native_entries(namespace)
}

/// Whether `body` opens with a fast-path prologue that reads the closure's frame as an
/// environment: it evaluates code there or defines variables in it, so such a closure gets no
/// slot frame, which would be reified on every call. Every other prologue reads its formals and
/// finishes the call through [`ClosureFrame`], which serves a slot frame without reifying it.
pub unsafe fn prologue_needs_environment(body: Sexp) -> bool {
    if body.kind() != SexpType::Language || body.cdr().kind() != SexpType::Pairlist {
        return false;
    }
    let first = body.cdr().car();
    if first.kind() != SexpType::Language || first.car() != symbols().dot_internal {
        return false;
    }
    let inner = first.cdr().car();
    inner.kind() == SexpType::Language
        && inner.car().kind() == SexpType::Symbol
        && ["ryMatchArg", "ryReduce", "ryTraceBack"].contains(&&*values::symbol_name(inner.car()))
}

/// Puts `.Internal(<internal_name>())` in front of the body of `function` when its formals and
/// body hash to `expected`, changing the closure in place. Returns whether the closure has the
/// prologue, which it may already have when it was saved in an image.
pub(crate) unsafe fn install_prologue(
    function: Sexp,
    name: &str,
    internal_name: &str,
    expected: u64,
    print: bool,
) -> R<bool> {
    if function.kind() != SexpType::Closure {
        return Ok(false);
    }
    let body = crate::bytecode::closure_body(function);
    let Some(block) = with_prologue(function.car(), body, name, internal_name, expected, print)?
    else {
        return Ok(false);
    };
    if block != body {
        crate::heap().set_cdr(function, block);
    }
    Ok(true)
}

/// `body` with `.Internal(<internal_name>())` in front when `formals` and `body` hash to
/// `expected`, or `body` itself when it opens with that prologue already; `None` otherwise.
unsafe fn with_prologue(
    formals: Sexp,
    body: Sexp,
    name: &str,
    internal_name: &str,
    expected: u64,
    print: bool,
) -> R<Option<Sexp>> {
    let prologue = values::lcons(
        symbols().dot_internal,
        values::cons(values::lcons(install(internal_name)?, nil())?, nil())?,
    )?;
    let mut original = body;
    let already = body.kind() == SexpType::Language
        && body.car() == symbols().brace
        && values::pairlist_length(body.cdr()) == 2
        && crate::sort::identical(body.cdr().car(), prologue, &IdenticalFlags::default());
    if already {
        original = body.cdr().cdr().car();
    }
    let mut hash = FNV_OFFSET;
    stable_hash(formals, &mut hash);
    stable_hash(original, &mut hash);
    if print {
        eprintln!("{name} {hash:#018x}");
    }
    if hash != expected {
        return Ok(None);
    }
    if already {
        return Ok(Some(body));
    }
    Ok(Some(values::lcons(
        symbols().brace,
        values::cons(prologue, values::cons(original, nil())?)?,
    )?))
}

/// Puts the prologue in front of the closure that `outer`'s body defines as
/// `inner <- function(...) ...`, by changing that `function` expression, so every closure the
/// outer function makes from it gets the fast path. Returns whether the expression has it.
unsafe fn install_nested_prologue(
    outer: Sexp,
    inner: &str,
    internal_name: &str,
    expected: u64,
    print: bool,
) -> R<bool> {
    if outer.kind() != SexpType::Closure {
        return Ok(false);
    }
    let Some(definition) =
        find_function_definition(crate::bytecode::closure_body(outer), install(inner)?)
    else {
        return Ok(false);
    };
    // `function(formals, body, srcref)`
    let body_cell = definition.cdr().cdr();
    let body = body_cell.car();
    let Some(block) =
        with_prologue(definition.cdr().car(), body, inner, internal_name, expected, print)?
    else {
        return Ok(false);
    };
    if block != body {
        crate::heap().set_car(body_cell, block);
    }
    Ok(true)
}

/// The `function(...)` expression assigned to `name` by an `name <- function(...)` anywhere in
/// `code`.
unsafe fn find_function_definition(code: Sexp, name: Sexp) -> Option<Sexp> {
    if code.kind() != SexpType::Language {
        return None;
    }
    if code.car() == symbols().assign
        && values::pairlist_length(code.cdr()) == 2
        && code.cdr().car() == name
    {
        let value = code.cdr().cdr().car();
        if value.kind() == SexpType::Language && value.car() == symbols().function {
            return Some(value);
        }
    }
    let mut rest = code.cdr();
    while rest.kind() == SexpType::Pairlist {
        if let Some(found) = find_function_definition(rest.car(), name) {
            return Some(found);
        }
        rest = rest.cdr();
    }
    None
}

/// The native code for a whole call of a base closure: the result, or `None` when the closure
/// is to run as usual. It sees the call's arguments as the closure would receive them.
pub type NativeEntry = unsafe fn(&[Arg]) -> R<Option<Sexp>>;

/// The base closures whose whole call runs natively when every argument it needs is already a
/// value, with the [`stable_hash`] of their formals and body (GNU R 4.6.2). Each is a one-line
/// body over primitives, so the frame, the promises and the lookups are most of its cost: in
/// testthat-style code `identical`, `isTRUE`, `nrow`, `%in%` and `%||%` each take 1 to 2.5% of
/// the run, and `typeof`, `inherits`, `is.factor`, `is.vector` and `isNamespace` are among the
/// closures called most often.
const NATIVE_ENTRIES: &[(&str, u64, NativeEntry)] = &[
    ("identical", 0x53ecb73e41a5f29e, native_identical),
    ("isTRUE", 0x092932cb0d38b378, native_is_true),
    ("isFALSE", 0x02a0aba295284434, native_is_false),
    ("nrow", 0xaf2f11bd0294734b, native_nrow),
    ("ncol", 0xcf3f0da3fff33d98, native_ncol),
    ("match", 0x8dee99370571e93c, native_match),
    ("%in%", 0x2fb6d82723e3191f, native_in),
    ("%||%", 0x3f61ec71bc0d9992, native_or_else),
    ("typeof", 0x6fcb4f267ff82404, native_typeof),
    ("inherits", 0x5adef86a3b8a8d6a, native_inherits),
    ("is.factor", 0x31be66479af8cf70, native_is_factor),
    ("is.data.frame", 0x7c700cf6459e2410, native_is_data_frame),
    ("is.vector", 0xf347f791e9374052, native_is_vector),
    ("isNamespace", 0x9dbd26992b319771, native_is_namespace),
    ("Sys.unsetenv", 0x3bf70db0fa5a60ee, native_sys_unsetenv),
    ("readRDS", 0x92a81976dbd4c35a, native_read_rds),
];

/// Marks the closures of [`NATIVE_ENTRIES`] whose code matches and records them in
/// [`interp::Interp::native_entries`]. The mark lives in the closure and survives the start-up
/// image, the registry does not, so an image start registers again.
pub unsafe fn register_native_entries(namespace: Sexp) -> R<()> {
    let print = std::env::var_os("RY_PRINT_FAST_PATH_HASHES").is_some();
    let bundled =
        crate::system::r_version().number() == crate::methods_core::BUNDLED_VERSION.trim();
    let mut missed = Vec::new();
    interp().native_entries.clear();
    for &(name, expected, entry) in NATIVE_ENTRIES {
        let function = eval::value_of(eval::find_var_in_frame(install(name)?, namespace)?)?;
        if function.kind() != SexpType::Closure {
            missed.push(name);
            continue;
        }
        let mut hash = FNV_OFFSET;
        stable_hash(function.car(), &mut hash);
        stable_hash(crate::bytecode::closure_body(function), &mut hash);
        if print {
            eprintln!("{name} {hash:#018x}");
        }
        if hash != expected {
            missed.push(name);
            continue;
        }
        function.set_gp_bits(gp::NATIVE_ENTRY, true);
        // The registry matches by identity, so the closure must outlive any reassignment of its
        // binding: a collected closure's address could be reused by a copy that carries the mark.
        crate::heap().preserve(function);
        interp().native_entries.push((function, entry));
    }
    if bundled && !missed.is_empty() {
        return Err(crate::unwind::error(format!(
            "the native entries for {} do not match ry's bundled base R; update NATIVE_ENTRIES \
             in base_fast_paths.rs (RY_PRINT_FAST_PATH_HASHES=1 prints the hashes)",
            missed.join(", ")
        )));
    }
    Ok(())
}

/// Runs the call of `function`, a closure marked [`gp::NATIVE_ENTRY`], natively when its
/// native code accepts the arguments. The closure's frame is never built, which is
/// unobservable because the native code only reads arguments that are already values: a
/// promise is read only when it is forced or when its code is a constant or a variable bound
/// to a value, so nothing runs that could see the missing frame (`sys.calls()`, a handler, a
/// traceback). A closure being debugged or traced runs as usual.
pub unsafe fn native_entry(function: Sexp, args: &[Arg]) -> R<Option<Sexp>> {
    if function.has_gp(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP | gp::CLOSURE_TRACE)
        || args.iter().any(|arg| arg.tag != nil())
    {
        return Ok(None);
    }
    let Some(&(_, entry)) = interp()
        .native_entries
        .iter()
        .find(|(candidate, _)| *candidate == function)
    else {
        // A closure with install-time machine code carries the same mark.
        let value = match crate::code_image::native_hooks() {
            Some(hooks) => (hooks.call)(function, args).transpose()?,
            None => None,
        };
        if value.is_some() && activity::enabled() {
            crate::eval::count_package_call(function, None, args);
        }
        return Ok(value);
    };
    // An entry whose closure returns invisibly clears the flag itself.
    interp().visible = true;
    let value = entry(args)?;
    if value.is_some() {
        activity::count(Counter::NativeEntries);
        if activity::enabled() {
            crate::eval::count_package_call(function, None, args);
        }
    }
    Ok(value)
}

/// Runs the call of `function`, a closure marked [`gp::NATIVE_ENTRY`], natively from its
/// unevaluated `arguments` when each is a constant or a variable bound to a value, so the call
/// builds no promise at all. Reading such an argument runs no R code, so reading it now gives
/// the value forcing its promise would give, and the native code reads only values; when it
/// declines, nothing has changed and the caller builds the promises as usual. `lookup` gives a
/// variable's value when reading it runs no R code ([`crate::effects::eager_argument`] in the
/// caller's environment, or the slots of a caller that has none).
pub unsafe fn native_entry_by_value(
    function: Sexp,
    arguments: Sexp,
    lookup: impl Fn(Sexp) -> Option<Sexp>,
) -> R<Option<Sexp>> {
    let mut args: SmallVec<[Arg; 4]> = SmallVec::new();
    let mut cell = arguments;
    while cell.kind() == SexpType::Pairlist {
        if cell.tag() != nil() {
            return Ok(None);
        }
        let expression = cell.car();
        let value = match expression.kind() {
            SexpType::Symbol => {
                if expression == missing_arg() || expression == symbols().dots {
                    return Ok(None);
                }
                match lookup(expression) {
                    Some(value) if value.kind() != SexpType::Promise => value,
                    _ => return Ok(None),
                }
            }
            SexpType::Language | SexpType::Promise | SexpType::Bytecode => return Ok(None),
            _ => expression,
        };
        args.push(Arg { tag: nil(), value });
        cell = cell.cdr();
    }
    native_entry(function, &args)
}

/// The value an argument stands for when reading it runs no R code: a value, a forced promise's
/// value, or the value of an unforced promise whose code is a constant or a variable bound to a
/// value. Reading such a promise settles it, as forcing would.
unsafe fn resolved(argument: Sexp) -> R<Option<Sexp>> {
    if argument.kind() != SexpType::Promise {
        return Ok((argument != missing_arg()).then_some(argument));
    }
    if argument.car() != values::unbound() {
        return Ok(Some(argument.car()));
    }
    if eval::is_reference(argument) {
        return match eval::reference_value(argument) {
            Some(value) => eval::settle_promise(argument, value).map(Some),
            None => Ok(None),
        };
    }
    // A promise over a slot frame without an environment names a placeholder that binds
    // nothing yet, so a lookup there would find the enclosure's variable instead.
    if argument.has_gp(gp::CHECKED_PROMISE) || argument.tag().has_gp(gp::PENDING_FRAME) {
        return Ok(None);
    }
    let code = argument.cdr();
    let value = match code.kind() {
        SexpType::Symbol if code != missing_arg() && code != symbols().dots => {
            match crate::effects::eager_argument(code, argument.tag()) {
                Some(value) if value.kind() != SexpType::Promise => value,
                _ => return Ok(None),
            }
        }
        SexpType::Symbol | SexpType::Language | SexpType::Promise | SexpType::Bytecode => {
            return Ok(None);
        }
        _ => code,
    };
    eval::settle_promise(argument, value).map(Some)
}

/// The values of exactly `N` arguments, all [`resolved`].
unsafe fn resolved_all<const N: usize>(args: &[Arg]) -> R<Option<[Sexp; N]>> {
    if args.len() != N {
        return Ok(None);
    }
    let mut values = [nil(); N];
    for (slot, arg) in values.iter_mut().zip(args) {
        match resolved(arg.value)? {
            Some(value) => *slot = value,
            None => return Ok(None),
        }
    }
    Ok(Some(values))
}

/// `identical(x, y)` with every flag at its default.
unsafe fn native_identical(args: &[Arg]) -> R<Option<Sexp>> {
    let Some([x, y]) = resolved_all::<2>(args)? else {
        return Ok(None);
    };
    let same = sort::identical(x, y, &IdenticalFlags::default());
    values::scalar_logical(same as i32).map(Some)
}

/// The one element of a logical scalar `x` that is not an object, whose `length()` and
/// `is.na()` therefore do not dispatch; `None` for an object, which runs the R body.
unsafe fn logical_scalar(x: Sexp) -> Option<Option<i32>> {
    if x.is_object() || x.is_altrep() {
        return None;
    }
    Some((x.kind() == SexpType::Logical && x.length() == 1).then(|| x.slice::<i32>()[0]))
}

/// `isTRUE(x)`: `is.logical(x) && length(x) == 1L && !is.na(x) && x`.
unsafe fn native_is_true(args: &[Arg]) -> R<Option<Sexp>> {
    let Some([x]) = resolved_all::<1>(args)? else {
        return Ok(None);
    };
    let Some(element) = logical_scalar(x) else {
        return Ok(None);
    };
    let truth = matches!(element, Some(value) if value != NA_LOGICAL && value != 0);
    values::scalar_logical(truth as i32).map(Some)
}

/// `isFALSE(x)`: `is.logical(x) && length(x) == 1L && !is.na(x) && !x`.
unsafe fn native_is_false(args: &[Arg]) -> R<Option<Sexp>> {
    let Some([x]) = resolved_all::<1>(args)? else {
        return Ok(None);
    };
    let Some(element) = logical_scalar(x) else {
        return Ok(None);
    };
    values::scalar_logical((element == Some(0)) as i32).map(Some)
}

/// `dim(x)[position]` for an `x` that is not an object, so `dim()` does not dispatch: `NULL`
/// without a `dim`, `NA` past its end.
unsafe fn dimension(args: &[Arg], position: usize) -> R<Option<Sexp>> {
    let Some([x]) = resolved_all::<1>(args)? else {
        return Ok(None);
    };
    if x.is_object() || crate::builtins::is_native_value(x) {
        return Ok(None);
    }
    let dim = values::get_attr(x, symbols().dim);
    if dim == nil() {
        return Ok(Some(nil()));
    }
    if dim.kind() != SexpType::Integer || dim.is_altrep() || values::has_attributes(dim) {
        return Ok(None);
    }
    let extent = values::ints(dim)
        .get(position)
        .copied()
        .unwrap_or(NA_INTEGER);
    values::scalar_integer(extent).map(Some)
}

/// `nrow(x)`: `dim(x)[1L]`.
unsafe fn native_nrow(args: &[Arg]) -> R<Option<Sexp>> {
    dimension(args, 0)
}

/// `ncol(x)`: `dim(x)[2L]`.
unsafe fn native_ncol(args: &[Arg]) -> R<Option<Sexp>> {
    dimension(args, 1)
}

/// `match(x, table)` with the default `nomatch` and `incomparables`.
unsafe fn native_match(args: &[Arg]) -> R<Option<Sexp>> {
    let Some([x, table]) = resolved_all::<2>(args)? else {
        return Ok(None);
    };
    match positions(x, table, NA_INTEGER)? {
        Some(Position::Scalar(position)) => values::scalar_integer(position).map(Some),
        Some(Position::Vector(positions)) => Ok(Some(positions)),
        None => Ok(None),
    }
}

/// `x %in% table`: `match(x, table, nomatch = 0L) > 0L`.
unsafe fn native_in(args: &[Arg]) -> R<Option<Sexp>> {
    let Some([x, table]) = resolved_all::<2>(args)? else {
        return Ok(None);
    };
    match positions(x, table, 0)? {
        Some(Position::Scalar(position)) => values::scalar_logical((position > 0) as i32).map(Some),
        Some(Position::Vector(positions)) => {
            let found = values::alloc(SexpType::Logical, length(positions))?;
            for (flag, position) in found
                .slice_mut::<i32>()
                .iter_mut()
                .zip(values::ints(positions))
            {
                *flag = (*position > 0) as i32;
            }
            Ok(Some(found))
        }
        None => Ok(None),
    }
}

/// `typeof(x)`: `.Internal(typeof(x))`.
unsafe fn native_typeof(args: &[Arg]) -> R<Option<Sexp>> {
    let Some([x]) = resolved_all::<1>(args)? else {
        return Ok(None);
    };
    crate::coerce::type_of(x).map(Some)
}

/// `inherits(x, what, which)` for `what` a character vector and an `x` that is not an S4
/// object, whose classes then come from its attributes without running R code.
unsafe fn native_inherits(args: &[Arg]) -> R<Option<Sexp>> {
    let (x, what, which) = match args.len() {
        2 => match resolved_all::<2>(args)? {
            Some([x, what]) => (x, what, None),
            None => return Ok(None),
        },
        3 => match resolved_all::<3>(args)? {
            Some([x, what, which]) => (x, what, Some(which)),
            None => return Ok(None),
        },
        _ => return Ok(None),
    };
    if x.has_gp(gp::S4_OBJECT) || what.kind() != SexpType::String {
        return Ok(None);
    }
    let which = which.is_some_and(|which| values::as_logical_scalar(which) == 1);
    crate::attribute_access::plain_inherits(x, what, which).map(Some)
}

/// `inherits(x, class)` for the closures whose whole body is that call, for an `x` that is not
/// an S4 object, whose classes are then those of its `class` attribute: an implicit class is
/// never `"factor"` or `"data.frame"`.
unsafe fn has_class(args: &[Arg], class: &str) -> R<Option<Sexp>> {
    let Some([x]) = resolved_all::<1>(args)? else {
        return Ok(None);
    };
    if x.has_gp(gp::S4_OBJECT) {
        return Ok(None);
    }
    values::scalar_bool(values::inherits(x, class)).map(Some)
}

/// `is.factor(x)`: `inherits(x, "factor")`.
unsafe fn native_is_factor(args: &[Arg]) -> R<Option<Sexp>> {
    has_class(args, "factor")
}

/// `is.data.frame(x)`: `inherits(x, "data.frame")`.
unsafe fn native_is_data_frame(args: &[Arg]) -> R<Option<Sexp>> {
    has_class(args, "data.frame")
}

/// `is.vector(x, mode)`: `.Internal(is.vector(x, mode))`, for a `mode` that is one string.
unsafe fn native_is_vector(args: &[Arg]) -> R<Option<Sexp>> {
    let (x, mode) = match args.len() {
        1 => match resolved_all::<1>(args)? {
            Some([x]) => {
                let mode = values::scalar_string("any")?;
                crate::heap().root(mode);
                (x, mode)
            }
            None => return Ok(None),
        },
        2 => match resolved_all::<2>(args)? {
            Some([x, mode]) => (x, mode),
            None => return Ok(None),
        },
        _ => return Ok(None),
    };
    if mode.kind() != SexpType::String || mode.length() != 1 || mode.is_object() {
        return Ok(None);
    }
    kernel(
        crate::coerce::do_is_vector,
        &[x, mode],
        ClosureFrame::Env(nil()),
    )
    .map(Some)
}

/// `isNamespace(ns)`: `.Internal(isNamespaceEnv(ns))`.
unsafe fn native_is_namespace(args: &[Arg]) -> R<Option<Sexp>> {
    let Some([ns]) = resolved_all::<1>(args)? else {
        return Ok(None);
    };
    values::scalar_bool(crate::envir::is_namespace(ns)).map(Some)
}

/// What [`positions`] found.
enum Position {
    /// The position of a one-element `x`, found without allocating.
    Scalar(i32),
    /// The match kernel's integer vector.
    Vector(Sexp),
}

/// `match(x, table, nomatch)` for `x` and `table` atomic vectors that are not objects, so
/// `match` neither calls `mtfrm` nor raises its error. A one-element `x` of `table`'s type is
/// looked up by a scan: an integer or logical by value (`NA` matches `NA`), a string by its
/// interned pointer when it is ASCII or `NA`, because interning gives equal ASCII text one
/// string whatever its declared encoding.
unsafe fn positions(x: Sexp, table: Sexp, nomatch: i32) -> R<Option<Position>> {
    let atomic = |value: Sexp| {
        !value.is_object()
            && matches!(
                value.kind(),
                SexpType::Logical
                    | SexpType::Integer
                    | SexpType::Real
                    | SexpType::Complex
                    | SexpType::String
            )
    };
    if !atomic(x) || !atomic(table) {
        return Ok(None);
    }
    if x.kind() == table.kind() && !x.is_altrep() && !table.is_altrep() && x.length() == 1 {
        let found = match x.kind() {
            SexpType::Integer | SexpType::Logical => {
                let wanted = x.slice::<i32>()[0];
                table.slice::<i32>().iter().position(|item| *item == wanted)
            }
            SexpType::String => {
                let wanted = x.slice::<Sexp>()[0];
                if wanted == values::na_string() || wanted.has_gp(gp::ASCII) {
                    table
                        .slice::<Sexp>()
                        .iter()
                        .position(|item| *item == wanted)
                } else {
                    return kernel_positions(x, table, nomatch);
                }
            }
            _ => return kernel_positions(x, table, nomatch),
        };
        return Ok(Some(Position::Scalar(
            found.map_or(nomatch, |index| index as i32 + 1),
        )));
    }
    kernel_positions(x, table, nomatch)
}

unsafe fn kernel_positions(x: Sexp, table: Sexp, nomatch: i32) -> R<Option<Position>> {
    let nomatch = values::scalar_integer(nomatch)?;
    crate::heap().root(nomatch);
    let positions = kernel(
        sort::do_match,
        &[x, table, nomatch, nil()],
        ClosureFrame::Env(nil()),
    )?;
    Ok(Some(Position::Vector(positions)))
}

/// `x %||% y`: `if (is.null(x)) y else x`, which reads `y` only when `x` is `NULL`.
unsafe fn native_or_else(args: &[Arg]) -> R<Option<Sexp>> {
    let [x, y] = args else {
        return Ok(None);
    };
    let Some(left) = resolved(x.value)? else {
        return Ok(None);
    };
    if left != nil() {
        return Ok(Some(left));
    }
    resolved(y.value)
}

/// `Sys.unsetenv(x)` of a character vector, which `as.character()` would leave as it is.
unsafe fn native_sys_unsetenv(args: &[Arg]) -> R<Option<Sexp>> {
    let Some([x]) = resolved_all::<1>(args)? else {
        return Ok(None);
    };
    if x.kind() != SexpType::String || x.is_object() {
        return Ok(None);
    }
    let value = kernel(
        crate::system::do_sys_unsetenv,
        &[x],
        ClosureFrame::Env(values::global_env()),
    )?;
    interp().visible = false;
    Ok(Some(value))
}

/// `readRDS(file)` for a path: the file read whole, inflated in one pass when it is gzip and
/// unserialized from memory. The R body reads through a `gzfile()` connection, which hands the
/// unserializer a few bytes at a time and costs about three times as much on the `Meta/*.rds`
/// and `.rdx` files every namespace load reads. It declines for a path `gzfile()` would expand
/// (`~`), a file it cannot open or read, an xz or bzip2 file, and a stream that does not
/// unserialize, so the R body raises those errors and warnings itself.
unsafe fn native_read_rds(args: &[Arg]) -> R<Option<Sexp>> {
    let Some([file]) = resolved_all::<1>(args)? else {
        return Ok(None);
    };
    if file.kind() != SexpType::String
        || file.is_object()
        || length(file) != 1
        || file.element(0) == values::na_string()
    {
        return Ok(None);
    }
    let path = file.element(0).bytes();
    if path.is_empty() || path.starts_with(b"~") {
        return Ok(None);
    }
    let Ok(path) = std::str::from_utf8(path) else {
        return Ok(None);
    };
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(None);
    };
    let inflated;
    let stream = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = flate2::read::MultiGzDecoder::new(bytes.as_slice());
        let mut output = Vec::with_capacity(bytes.len() * 4);
        if std::io::Read::read_to_end(&mut decoder, &mut output).is_err() {
            return Ok(None);
        }
        inflated = output;
        inflated.as_slice()
    } else if bytes.starts_with(b"BZh") || bytes.starts_with(b"\xFD7zXZ") {
        return Ok(None);
    } else {
        bytes.as_slice()
    };
    // `unserialize()` of memory also takes `save()` files, which `readRDS()` rejects.
    if !(stream.starts_with(b"X\n") || stream.starts_with(b"B\n") || stream.starts_with(b"A\n"))
    {
        return Ok(None);
    }
    match crate::serialize::unserialize(stream, nil()) {
        Ok(value) => {
            interp().visible = true;
            Ok(Some(value))
        }
        // The R body reads the same stream and reports the error against its own call.
        Err(Unwind::Error(_)) => Ok(None),
        Err(other) => Err(other),
    }
}

/// The prologue's result when the R body is to run.
pub(crate) fn decline() -> R<Sexp> {
    Ok(nil())
}

/// The frame of the closure whose prologue is running: its environment, or a slot frame that
/// has none, whose formals the helpers below read from its slots so the frame stays without one.
#[derive(Clone, Copy)]
pub(crate) enum ClosureFrame {
    Env(Sexp),
    Slots(usize),
}

impl ClosureFrame {
    /// The frame the builtin `call` was made from.
    pub(crate) unsafe fn of(call: &Call<'_>) -> ClosureFrame {
        if call.rho != nil() || call.frame == crate::NO_SLOTS {
            ClosureFrame::Env(call.rho)
        } else {
            ClosureFrame::Slots(call.frame as usize)
        }
    }

    /// The slot frame's position while it has no environment, or the environment it has, which
    /// a slot frame may have made since the call began (forcing an argument can make it).
    unsafe fn resolve(self) -> Result<usize, Sexp> {
        match self {
            ClosureFrame::Env(env) => Err(env),
            ClosureFrame::Slots(index) => match interp().frames[index].env {
                env if env == nil() => Ok(index),
                env => Err(env),
            },
        }
    }
}

/// Finishes the call of the closure whose frame is `frame` with `value`, as `return(value)`.
pub(crate) unsafe fn finish(frame: ClosureFrame, value: Sexp, visible: bool) -> R<Sexp> {
    let env = match frame.resolve() {
        Err(env) => env,
        Ok(index) => eval::pending_env(index)?,
    };
    interp().visible = visible;
    Err(jump(Jump::Return { env, value }))
}

/// The value of the formal `name` in `frame`, supplied or defaulted, forced as evaluating the
/// name would; `None` when it has neither a value nor a default.
pub(crate) unsafe fn argument(frame: ClosureFrame, symbol: Sexp) -> R<Option<Sexp>> {
    let frame = match frame.resolve() {
        Err(env) => env,
        Ok(index) => {
            let Some((value, _)) = eval::slot_formal(index, symbol) else {
                return Ok(None);
            };
            if value == missing_arg() || value == values::unbound() {
                return Ok(None);
            }
            let value = if value.kind() == SexpType::Promise {
                eval::force_promise(value)?
            } else {
                value
            };
            crate::heap().root(value);
            return Ok(Some(value));
        }
    };
    match crate::heap().frame_cell(frame, symbol) {
        Some(cell) if cell.cdr() != missing_arg() => {}
        _ => return Ok(None),
    }
    let value = eval::eval_symbol(symbol, frame)?;
    crate::heap().root(value);
    Ok(Some(value))
}

/// Whether the formal `name` holds its own default, untouched: no value at all, or the default
/// the call bound because the argument was left out. An argument the caller passed on from its
/// own missing formal is not a default, because forcing it raises an error.
unsafe fn is_default(frame: ClosureFrame, symbol: Sexp) -> R<bool> {
    let frame = match frame.resolve() {
        Err(env) => env,
        Ok(index) => {
            return Ok(eval::slot_formal(index, symbol)
                .is_none_or(|(value, missing)| missing || value == missing_arg()));
        }
    };
    let Some(cell) = crate::heap().frame_cell(frame, symbol) else {
        return Ok(true);
    };
    Ok(cell.cdr() == missing_arg() || cell.has_gp(gp::BINDING_MISSING))
}

/// Whether the call supplied the formal `name`, as `missing(name)` is false.
unsafe fn is_supplied(frame: ClosureFrame, symbol: Sexp) -> R<bool> {
    match frame.resolve() {
        Ok(index) => match eval::slot_formal(index, symbol) {
            Some((value, missing)) => Ok(!missing && !eval::value_is_missing(value)?),
            None => Ok(!eval::is_missing(
                symbol,
                eval::frame_env(index, eval::Reify::Site("missing()"))?,
            )?),
        },
        Err(env) => Ok(!eval::is_missing(symbol, env)?),
    }
}

/// The elements of the frame's `...`, unforced; `None` when one is an empty argument.
unsafe fn dots(frame: ClosureFrame) -> R<Option<SmallVec<[(Sexp, Sexp); 8]>>> {
    let list = match frame.resolve() {
        Ok(index) => eval::slot_dots(index).unwrap_or(nil()),
        Err(env) => match crate::heap().frame_cell(env, symbols().dots) {
            Some(cell) => cell.cdr(),
            None => nil(),
        },
    };
    if list.kind() != SexpType::Dots {
        return Ok(Some(SmallVec::new()));
    }
    let mut items = SmallVec::new();
    let mut item = list;
    while item.kind() == SexpType::Dots || item.kind() == SexpType::Pairlist {
        if item.car() == missing_arg() {
            return Ok(None);
        }
        items.push((item.tag(), item.car()));
        item = item.cdr();
    }
    Ok(Some(items))
}

/// Calls the builtin `function` with positional `args` from `frame`, for a kernel that reads
/// only its arguments.
pub(crate) unsafe fn kernel(function: BuiltinFn, args: &[Sexp], frame: ClosureFrame) -> R<Sexp> {
    let args: SmallVec<[Arg; 9]> = args
        .iter()
        .map(|&value| Arg { tag: nil(), value })
        .collect();
    let (rho, frame) = match frame {
        ClosureFrame::Env(env) => (env, crate::NO_SLOTS),
        ClosureFrame::Slots(index) => (nil(), index as u32),
    };
    let call = Call {
        call: nil(),
        op: nil(),
        args: &args,
        rho,
        frame,
        reusable: None,
    };
    let value = function(&call)?;
    crate::heap().root(value);
    Ok(value)
}

/// A plain vector: atomic, with no class and no `dim`, which the set operations and `format`
/// treat without dispatch.
unsafe fn is_plain_atomic(value: Sexp) -> bool {
    matches!(
        value.kind(),
        SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::String
    ) && !value.is_object()
        && values::get_attr(value, symbols().dim) == nil()
}

unsafe fn is_ascii(text: Sexp) -> bool {
    text != values::na_string() && text.bytes().is_ascii()
}

/// `match.arg(arg, choices, several.ok = FALSE)` for one string, or for `arg` left at the
/// choices themselves. With `choices` missing it evaluates the caller's default for `arg` in
/// the caller's frame, as the R body does through `sys.function(sys.parent())`.
unsafe fn do_match_arg(call: &Call<'_>) -> R<Sexp> {
    let frame = call.env()?;
    if !is_default(ClosureFrame::Env(frame), symbols().several_ok)? {
        return decline();
    }
    // The R body evaluates a missing `choices` before it forces `arg`, and a supplied one after.
    let defaulted = if is_supplied(ClosureFrame::Env(frame), symbols().choices)? {
        None
    } else {
        let Some(choices) = default_choices(frame)? else {
            return decline();
        };
        Some(choices)
    };
    let Some(arg) = argument(ClosureFrame::Env(frame), symbols().arg)? else {
        return decline();
    };
    if arg.kind() != SexpType::String || arg.attrib() != nil() {
        return decline();
    }
    let choices = match defaulted {
        Some(choices) => choices,
        None => match argument(ClosureFrame::Env(frame), symbols().choices)? {
            Some(choices) => choices,
            None => return decline(),
        },
    };
    if choices.kind() != SexpType::String || choices.attrib() != nil() {
        return decline();
    }
    let result = values::alloc(SexpType::String, 1)?;
    if length(arg) >= 1 && sort::identical(arg, choices, &IdenticalFlags::default()) {
        crate::heap().set_element(result, 0, arg.element(0));
        return finish(ClosureFrame::Env(frame), result, true);
    }
    if length(arg) != 1 {
        return decline();
    }
    let target = arg.element(0);
    if !is_ascii(target) || target.bytes().is_empty() {
        return decline();
    }
    let count = length(choices);
    let mut found = None;
    for index in 0..count {
        let choice = choices.element(index);
        if !is_ascii(choice) {
            return decline();
        }
        if choice.bytes() == target.bytes() {
            found = Some(index);
            break;
        }
    }
    if found.is_none() {
        let mut partial =
            (0..count).filter(|&index| choices.element(index).bytes().starts_with(target.bytes()));
        if let (Some(index), None) = (partial.next(), partial.next()) {
            found = Some(index);
        }
    }
    let Some(index) = found else {
        return decline();
    };
    crate::heap().set_element(result, 0, choices.element(index));
    finish(ClosureFrame::Env(frame), result, true)
}

/// The default of the caller's formal that `arg` names, evaluated in the caller's frame, when
/// that default is a character constant or a `c()` of character constants. It binds `choices`
/// in the frame as the R body does, so a declined fast path does not evaluate it a second time.
unsafe fn default_choices(frame: Sexp) -> R<Option<Sexp>> {
    let Some(cell) = crate::heap().frame_cell(frame, symbols().arg) else {
        return Ok(None);
    };
    let promise = cell.cdr();
    if promise.kind() != SexpType::Promise || promise.cdr().kind() != SexpType::Symbol {
        return Ok(None);
    }
    let name = promise.cdr();
    let Some((function, caller)) = crate::envir::sys_parent_of(frame)? else {
        return Ok(None);
    };
    if function.kind() != SexpType::Closure {
        return Ok(None);
    }
    let mut formal = function.car();
    while formal.kind() == SexpType::Pairlist && formal.tag() != name {
        formal = formal.cdr();
    }
    if formal.kind() != SexpType::Pairlist {
        return Ok(None);
    }
    let default = formal.car();
    let is_text = |value: Sexp| value.kind() == SexpType::String && value.attrib() == nil();
    let constant = is_text(default)
        || (default.kind() == SexpType::Language && default.car() == symbols().c && {
            let mut item = default.cdr();
            let mut all = true;
            while item.kind() == SexpType::Pairlist {
                all &= item.tag() == nil() && is_text(item.car());
                item = item.cdr();
            }
            all
        });
    if !constant {
        return Ok(None);
    }
    let choices = eval::eval(default, caller)?;
    crate::heap().root(choices);
    eval::define(symbols().choices, choices, frame)?;
    Ok(Some(choices))
}

/// `stopifnot(...)` when every expression is all `TRUE`. The first that is not leaves the R body
/// to build the error from its call.
unsafe fn do_stopifnot(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    if is_supplied(frame, symbols().exprs)? || is_supplied(frame, symbols().expr_object)? {
        return decline();
    }
    let Some(items) = dots(frame)? else {
        return decline();
    };
    for (_, item) in items {
        let value = eval::value_of(item)?;
        if value.kind() != SexpType::Logical || value.is_object() {
            return decline();
        }
        let all_true = values::ints(value).iter().all(|&element| element == 1);
        crate::unwind::raise_parked()?;
        if !all_true {
            return decline();
        }
    }
    finish(frame, nil(), false)
}

/// `Reduce(f, x, init, right, accumulate)` over a plain vector and with `f` a closure or a
/// builtin. The R body calls `forceAndCall(2, f, init, x[[i]])` per element, which builds a
/// call and evaluates `x[[i]]` through the evaluator each time; this loop reads the element
/// directly and passes both arguments as forced promises of the same expressions, so `f` sees
/// the same call, the same `substitute()` results and the same frame variables `init` and `i`.
unsafe fn do_reduce(call: &Call<'_>) -> R<Sexp> {
    let frame = call.env()?;
    let missing_init = !is_supplied(ClosureFrame::Env(frame), symbols().init)?;
    let Some(x) = argument(ClosureFrame::Env(frame), symbols().x)? else {
        return decline();
    };
    // `is.vector(x) && !is.object(x)`, and `length(x)` cannot dispatch.
    let plain = matches!(
        x.kind(),
        SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::String
            | SexpType::Raw
            | SexpType::List
    ) && !x.is_object()
        && !x.is_altrep()
        && only_names(x);
    let count = length(x);
    if !plain || count == 0 {
        return decline();
    }
    // `match.fun(f)` returns a function as it is.
    let Some(function) = argument(ClosureFrame::Env(frame), symbols().f)? else {
        return decline();
    };
    if !matches!(function.kind(), SexpType::Closure | SexpType::Builtin) {
        return decline();
    }
    // The R body reads `right` before `accumulate` when `init` is missing, and after otherwise.
    // A flag that is not TRUE or FALSE stops the R body with an error before it reads the
    // other one, so the prologue declines before forcing it.
    let (right, accumulate) = if missing_init {
        let Some(right) = flag(ClosureFrame::Env(frame), symbols().right)? else {
            return decline();
        };
        (right, flag(ClosureFrame::Env(frame), symbols().accumulate)?)
    } else {
        let Some(accumulate) = flag(ClosureFrame::Env(frame), symbols().accumulate)? else {
            return decline();
        };
        let Some(right) = flag(ClosureFrame::Env(frame), symbols().right)? else {
            return decline();
        };
        (right, Some(accumulate))
    };
    let Some(accumulate) = accumulate else {
        return decline();
    };
    let symbols = symbols();
    let heap = crate::heap();
    let init_symbol = symbols.init;
    let i_symbol = symbols.i;
    eval::define(
        symbols.mis,
        values::scalar_logical(missing_init as i32)?,
        frame,
    )?;
    eval::define(symbols.len, values::scalar_integer(count as i32)?, frame)?;
    eval::define(symbols.f, function, frame)?;
    let element_call = values::call(symbols.bracket2, &[symbols.x, i_symbol])?;
    heap.root(element_call);
    let arguments = if right {
        [element_call, init_symbol]
    } else {
        [init_symbol, element_call]
    };
    let inner_call = values::call(symbols.f, &arguments)?;
    heap.root(inner_call);
    let positions = if !missing_init {
        0..count
    } else if right {
        0..count - 1
    } else {
        1..count
    };
    if missing_init {
        let first = if right { count } else { 1 };
        let index = values::scalar_integer(first as i32)?;
        heap.root(index);
        let value = crate::subset::subset2(x, &[index], Some(true), element_call)?;
        eval::define(init_symbol, value, frame)?;
    } else if argument(ClosureFrame::Env(frame), symbols.init)?.is_none() {
        return decline();
    }
    // With `accumulate`, `out` holds `len` slots and `out[[slot]] <- init` stores the running
    // value. The store writes the slot directly while `out` is unshared and `init` is not NULL
    // (which deletes the slot), and evaluates the assignment itself otherwise.
    let out_symbol = values::install("out")?;
    let slots = positions.len() + 1;
    let store = |slot: usize| -> R<()> {
        let value = eval::value_of(eval::find_var_in_frame(init_symbol, frame)?)?;
        let out = eval::value_of(eval::find_var_in_frame(out_symbol, frame)?)?;
        if value != nil()
            && out.kind() == SexpType::List
            && !out.is_altrep()
            && !values::maybe_shared(out)
            && slot <= length(out)
        {
            crate::heap().set_element(out, slot - 1, value);
            return Ok(());
        }
        let slot = values::scalar_integer(slot as i32)?;
        crate::heap().root(slot);
        let target = values::call(symbols.bracket2, &[out_symbol, slot])?;
        crate::heap().root(target);
        let assignment = values::call(symbols.assign, &[target, init_symbol])?;
        crate::heap().root(assignment);
        eval::eval(assignment, frame).map(drop)
    };
    if accumulate {
        eval::define(symbols.len, values::scalar_integer(slots as i32)?, frame)?;
        eval::define(out_symbol, values::alloc(SexpType::List, slots)?, frame)?;
        if right {
            store(slots)?;
        } else if missing_init {
            store(1)?;
        }
    }
    // `i` is bound to one integer updated in place, as `lapply` does, until `f` keeps or
    // rebinds it (its reference count changes), and a list's elements are read directly.
    let mut index = values::scalar_integer(0)?;
    heap.root(index);
    eval::define(i_symbol, index, frame)?;
    let mut bound_refcount = index.refcount();
    let mut step = |position: usize| -> R<()> {
        if index.refcount() != bound_refcount {
            index = values::scalar_integer(0)?;
            crate::heap().root(index);
            eval::define(i_symbol, index, frame)?;
            bound_refcount = index.refcount();
        }
        index.slice_mut::<i32>()[0] = position as i32 + 1;
        let element = if x.kind() == SexpType::List {
            x.element(position)
        } else {
            crate::subset::subset2(x, &[index], Some(true), element_call)?
        };
        crate::heap().root(element);
        // `for (i in ind) { out[[i]] <- init; init <- f(init, x[[i]]) }` stores before calling.
        let store_before = accumulate && !missing_init && !right;
        if store_before {
            store(position + 1)?;
        }
        let init = eval::find_var_in_frame(init_symbol, frame)?;
        let init = eval::value_of(init)?;
        let values = if right {
            [element, init]
        } else {
            [init, element]
        };
        let value = eval::call_with_values(inner_call, function, &values, frame)?;
        eval::define(init_symbol, value, frame)?;
        if accumulate && !store_before {
            store(position + 1)?;
        }
        Ok(())
    };
    if right {
        for position in positions.rev() {
            crate::scope(|| step(position))?;
        }
    } else {
        for position in positions {
            crate::scope(|| step(position))?;
        }
    }
    if !accumulate {
        let value = eval::value_of(eval::find_var_in_frame(init_symbol, frame)?)?;
        return finish(ClosureFrame::Env(frame), value, true);
    }
    if !missing_init && !right {
        store(slots)?;
    }
    let out = eval::value_of(eval::find_var_in_frame(out_symbol, frame)?)?;
    // `if (all(lengths(out) == 1L)) out <- unlist(out, recursive = FALSE)`: with no element an
    // object (whose `length` and `unlist` could dispatch) the test reads lengths directly.
    let plain = out.kind() == SexpType::List
        && !out.is_object()
        && (0..length(out)).all(|position| {
            let element = out.element(position);
            (element.kind().is_atomic() || element.kind() == SexpType::List)
                && !element.is_object()
        });
    if !plain {
        let tail = crate::lower::parse_text(
            "if (all(lengths(out) == 1L)) out <- unlist(out, recursive = FALSE)\nout",
            "<Reduce>",
        )?;
        crate::heap().root(tail);
        let mut value = nil();
        for position in 0..length(tail) {
            value = eval::eval(tail.element(position), frame)?;
        }
        return finish(ClosureFrame::Env(frame), value, true);
    }
    if (0..length(out)).all(|position| length(out.element(position)) == 1) {
        let no = values::scalar_bool(false)?;
        crate::heap().root(no);
        let yes = values::scalar_bool(true)?;
        crate::heap().root(yes);
        let value = kernel(crate::combine::do_unlist, &[out, no, yes], ClosureFrame::Env(frame))?;
        eval::define(out_symbol, value, frame)?;
        return finish(ClosureFrame::Env(frame), value, true);
    }
    finish(ClosureFrame::Env(frame), out, true)
}

/// Whether `value` has no attribute other than `names`, as `is.vector` requires.
unsafe fn only_names(value: Sexp) -> bool {
    let mut cell = value.attrib();
    while cell.kind() == SexpType::Pairlist {
        if cell.tag() != symbols().names {
            return false;
        }
        cell = cell.cdr();
    }
    true
}

/// The formal `name` forced as `if (name)` reads it, when it is a single `TRUE` or `FALSE`.
unsafe fn flag(frame: ClosureFrame, symbol: Sexp) -> R<Option<bool>> {
    let Some(value) = argument(frame, symbol)? else {
        return Ok(None);
    };
    if value.kind() != SexpType::Logical || length(value) != 1 || value.attrib() != nil() {
        return Ok(None);
    }
    Ok(match values::ints(value)[0] {
        NA_LOGICAL => None,
        flag => Some(flag != 0),
    })
}

/// `setdiff(x, y)` of plain vectors: `unique(x)` without names, less the elements `y` has.
unsafe fn do_setdiff(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    let Some(x) = argument(frame, symbols().x)? else {
        return decline();
    };
    if x == nil() {
        return finish(frame, nil(), true);
    }
    let Some(y) = argument(frame, symbols().y)? else {
        return decline();
    };
    if !is_plain_atomic(x) || !(y == nil() || is_plain_atomic(y)) {
        return decline();
    }
    let no = values::scalar_bool(false)?;
    crate::heap().root(no);
    let none = values::logical_vector(&[NA_LOGICAL])?;
    crate::heap().root(none);
    let unique_x = kernel(sort::do_unique, &[x, no, no, none], frame)?;
    values::set_attr(unique_x, symbols().names, nil())?;
    let unique_y = kernel(sort::do_unique, &[y, no, no, none], frame)?;
    let zero = values::scalar_integer(0)?;
    crate::heap().root(zero);
    let positions = kernel(sort::do_match, &[unique_x, unique_y, zero, nil()], frame)?;
    let kept: Vec<usize> = values::ints(positions)
        .iter()
        .enumerate()
        .filter_map(|(index, &position)| (position == 0).then_some(index))
        .collect();
    let result = if kept.len() == length(unique_x) {
        unique_x
    } else {
        crate::subset::select_elements(unique_x, &kept)?
    };
    finish(frame, result, true)
}

/// `intersect(x, y)` of plain vectors: the elements of `unique(x)` without names that `y` has,
/// in the type `c()` gives `x` and `y` together. R's body asks `.set_ops_need_as_vector()`
/// first, which costs two `tryCatch()` calls, and for plain vectors its answer cannot change the
/// result because `unique()` drops every attribute.
unsafe fn do_intersect(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    let Some(x) = argument(frame, symbols().x)? else {
        return decline();
    };
    if x == nil() {
        return finish(frame, nil(), true);
    }
    let Some(y) = argument(frame, symbols().y)? else {
        return decline();
    };
    if y == nil() {
        return finish(frame, nil(), true);
    }
    if !is_plain_atomic(x) || !is_plain_atomic(y) {
        return decline();
    }
    // `c()` turns numbers into strings with its own formatting, and complex numbers have their
    // own rules; those mixes keep the R body.
    let mixed = x.kind() != y.kind();
    let text_or_complex = |kind: SexpType| matches!(kind, SexpType::String | SexpType::Complex);
    if mixed && (text_or_complex(x.kind()) || text_or_complex(y.kind())) {
        return decline();
    }
    let no = values::scalar_bool(false)?;
    crate::heap().root(no);
    let none = values::logical_vector(&[NA_LOGICAL])?;
    crate::heap().root(none);
    let unique_x = kernel(sort::do_unique, &[x, no, no, none], frame)?;
    values::set_attr(unique_x, symbols().names, nil())?;
    let unique_y = kernel(sort::do_unique, &[y, no, no, none], frame)?;
    let zero = values::scalar_integer(0)?;
    crate::heap().root(zero);
    let positions = kernel(sort::do_match, &[unique_x, unique_y, zero, nil()], frame)?;
    let kept: Vec<usize> = values::ints(positions)
        .iter()
        .enumerate()
        .filter_map(|(index, &position)| (position > 0).then_some(index))
        .collect();
    let mut result = if kept.len() == length(unique_x) {
        unique_x
    } else {
        crate::subset::select_elements(unique_x, &kept)?
    };
    // Logical, integer and double rank in that order for `c()`.
    let rank = |kind: SexpType| match kind {
        SexpType::Logical => 0,
        SexpType::Integer => 1,
        _ => 2,
    };
    if mixed && rank(y.kind()) > rank(x.kind()) {
        crate::heap().root(result);
        result = crate::coerce::coerce_vector(result, y.kind())?;
    }
    finish(frame, result, true)
}

/// `Sys.getenv(x, unset, names)` of a character vector `x`, a single string or `NA` for
/// `unset`, and a single logical for `names`: the values, named by `x` when `names` asks for
/// it, without the closure's `structure()` call.
unsafe fn do_sys_getenv(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    let symbols = symbols();
    let Some(x) = argument(frame, symbols.x)? else {
        return decline();
    };
    // An empty `x` lists the whole environment, which `structure()` names with `NA`s.
    if x.kind() != SexpType::String || x.attrib() != nil() || length(x) == 0 {
        return decline();
    }
    let Some(unset) = argument(frame, symbols.unset)? else {
        return decline();
    };
    if length(unset) != 1 || unset.attrib() != nil() {
        return decline();
    }
    let unset = match unset.kind() {
        SexpType::String => unset,
        SexpType::Logical if values::ints(unset)[0] == NA_LOGICAL => {
            let text = values::scalar_string_charsxp(values::na_string())?;
            crate::heap().root(text);
            text
        }
        _ => return decline(),
    };
    let Some(names) = argument(frame, symbols.names)? else {
        return decline();
    };
    if names.kind() != SexpType::Logical || length(names) != 1 || names.attrib() != nil() {
        return decline();
    }
    let named = match values::ints(names)[0] {
        NA_LOGICAL => length(x) > 1,
        flag => flag != 0,
    };
    let value = kernel(crate::system::do_sys_getenv, &[x, unset], frame)?;
    if named {
        values::set_attr(value, symbols.names, x)?;
    }
    finish(frame, value, true)
}

/// `Sys.setenv(...)` where every argument is named and is a single string, so `unlist()` and
/// `as.character()` leave the values as they are.
unsafe fn do_sys_setenv(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    let Some(items) = dots(frame)? else {
        return decline();
    };
    if items.is_empty() || items.iter().any(|&(tag, _)| tag == nil()) {
        return decline();
    }
    let mut new_values: SmallVec<[Sexp; 8]> = SmallVec::new();
    // `list(...)` forces every element before any is looked at.
    for &(_, item) in &items {
        let value = eval::value_of(item)?;
        crate::heap().root(value);
        new_values.push(value);
    }
    if new_values.iter().any(|&value| {
        value.kind() != SexpType::String || length(value) != 1 || value.attrib() != nil()
    }) {
        return decline();
    }
    let names = values::alloc(SexpType::String, items.len())?;
    crate::heap().root(names);
    let strings = values::alloc(SexpType::String, items.len())?;
    crate::heap().root(strings);
    for (index, (&(tag, _), &value)) in items.iter().zip(&new_values).enumerate() {
        let name = tag.car();
        if name.bytes().is_empty() {
            return decline();
        }
        values::set_elt(names, index, name);
        values::set_elt(strings, index, value.element(0));
    }
    let value = kernel(crate::system::do_sys_setenv, &[names, strings], frame)?;
    finish(frame, value, false)
}

/// Attribute names `structure()` sets through `attributes<-` with no check that could fail or
/// convert the object: everything but the attributes R validates.
fn sets_unchecked(name: &[u8]) -> bool {
    !matches!(
        name,
        b".Dim"
            | b".Dimnames"
            | b".Names"
            | b".Tsp"
            | b".Label"
            | b"dim"
            | b"dimnames"
            | b"names"
            | b"tsp"
            | b"levels"
            | b"comment"
            | b"row.names"
            | b"class"
    )
}

/// `structure(.Data, ...)` when every attribute is named and is either a plain one, `names` that
/// need no coercion, or a `class` that is a character vector without `"factor"`.
unsafe fn do_structure(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    let Some(data) = argument(frame, symbols().dot_data)? else {
        return decline();
    };
    if data == nil() {
        return decline();
    }
    let Some(items) = dots(frame)? else {
        return decline();
    };
    if items.is_empty() {
        return finish(frame, data, true);
    }
    let mut attributes: SmallVec<[(Sexp, Sexp); 8]> = SmallVec::new();
    for &(tag, _) in &items {
        if tag == nil() {
            return decline();
        }
    }
    // `list(...)` forces every element before any is looked at.
    for &(tag, item) in &items {
        let value = eval::value_of(item)?;
        attributes.push((tag, value));
    }
    for &(tag, value) in &attributes {
        let name = values::symbol_name(tag);
        if name.as_bytes() == b"class" {
            let factor = value.kind() == SexpType::String
                && (0..length(value)).any(|index| value.element(index).bytes() == b"factor");
            // The R body's `any(class == "factor")` fails on an NA element.
            let na = value.kind() == SexpType::String
                && (0..length(value)).any(|index| value.element(index) == values::na_string());
            if value.kind() != SexpType::String || value.attrib() != nil() || factor || na {
                return decline();
            }
        } else if name.as_bytes() == b"names" {
            // `names<-` coerces, pads a shorter value with NA and rejects a longer one; a plain
            // character vector of the vector's own length needs none of that.
            let plain = value == nil()
                || (value.kind() == SexpType::String
                    && value.attrib() == nil()
                    && length(value) == length(data));
            let vector = matches!(
                data.kind(),
                SexpType::Logical
                    | SexpType::Integer
                    | SexpType::Real
                    | SexpType::Complex
                    | SexpType::String
                    | SexpType::Raw
                    | SexpType::List
                    | SexpType::Expression
            );
            if !plain || !vector {
                return decline();
            }
        } else if !sets_unchecked(name.as_bytes()) {
            return decline();
        }
    }
    // `attributes(.Data) <- c(attributes(.Data), list(...))` clears the attributes and adds them
    // back with `dim` first, so setting the new ones in place is the same unless that moves an
    // existing `dim`. `attributes()` lists `names` first, so its position cannot be observed.
    let symbols = symbols();
    let mut before_dim = false;
    let mut cell = data.attrib();
    while cell != nil() {
        if cell.tag() == symbols.dim && before_dim {
            return decline();
        }
        before_dim |= cell.tag() != symbols.names;
        cell = cell.cdr();
    }
    let result = crate::subset::modifiable(data)?;
    crate::heap().root(result);
    for &(tag, value) in &attributes {
        crate::subset::set_attribute_checked(result, tag, value, call.call)?;
    }
    finish(frame, result, true)
}

/// `format.default(x, ...)` of a logical, numeric or complex `x` with every mark, `justify`,
/// `zero.print` and `drop0trailing` left at its default, where `prettyNum()` returns what the
/// `format` internal made unchanged.
unsafe fn do_format_default(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    let symbols = symbols();
    for symbol in [
        symbols.justify,
        symbols.big_mark,
        symbols.big_interval,
        symbols.small_mark,
        symbols.small_interval,
        symbols.zero_print,
        symbols.drop0trailing,
    ] {
        if !is_default(frame, symbol)? {
            return decline();
        }
    }
    match dots(frame)? {
        Some(items) if items.is_empty() => {}
        _ => return decline(),
    }
    let Some(x) = argument(frame, symbols.x)? else {
        return decline();
    };
    if !matches!(
        x.kind(),
        SexpType::Logical | SexpType::Integer | SexpType::Real | SexpType::Complex
    ) {
        return decline();
    }
    let mut args: SmallVec<[Sexp; 9]> = SmallVec::new();
    args.push(x);
    for symbol in [symbols.trim, symbols.digits, symbols.nsmall, symbols.width] {
        let Some(value) = argument(frame, symbol)? else {
            return decline();
        };
        args.push(value);
    }
    let justify_none = values::scalar_integer(3)?;
    crate::heap().root(justify_none);
    args.push(justify_none);
    for symbol in [symbols.na_encode, symbols.scientific, symbols.decimal_mark] {
        let Some(value) = argument(frame, symbol)? else {
            return decline();
        };
        args.push(value);
    }
    let decimal_mark = args[8];
    // `prettyNum()` warns when the decimal mark equals the empty big mark.
    if decimal_mark.kind() != SexpType::String
        || decimal_mark.is_object()
        || length(decimal_mark) != 1
        || decimal_mark.element(0) == values::na_string()
        || decimal_mark.element(0).bytes().is_empty()
    {
        return decline();
    }
    // A bad argument makes the internal fail; the R body then reports it from its own call.
    let result = match kernel(crate::format::do_format, &args, frame) {
        Ok(result) => result,
        Err(Unwind::Error(_)) => return decline(),
        Err(other) => return Err(other),
    };
    finish(frame, result, true)
}

/// `assignNativeRoutines(dll, lib, env, nativeRoutines)`, the closure `loadNamespace()` defines
/// to bind a package's registered native routines in its namespace (`useDynLib(pkg,
/// .registration = TRUE)`): each routine's `NativeSymbolInfo` under its name with the
/// registration prefix and suffix, and as the value the routine names named by those variables.
/// The R body grows both vectors with `c()` once per routine inside two nested `lapply()`
/// calls, about a tenth of a package's load. It declines, before any effect, where the R body
/// warns (a name already bound, or bound twice), and when the namespace file lists symbols by
/// name, which `getNativeSymbolInfo()` resolves.
unsafe fn do_assign_native_routines(call: &Call<'_>) -> R<Sexp> {
    let frame = ClosureFrame::of(call);
    let Some(native_routines) = argument(frame, install("nativeRoutines")?)? else {
        return decline();
    };
    // `parseNamespaceFile()` makes it with the class `NativeRoutineMap`, which has no methods.
    let class = values::class_attr(native_routines);
    if native_routines.kind() != SexpType::List
        || (class != nil()
            && (length(class) != 1 || class.element(0).bytes() != b"NativeRoutineMap"))
    {
        return decline();
    }
    if length(native_routines) == 0 {
        return finish(frame, values::alloc(SexpType::String, 0)?, true);
    }
    let use_registration = values::list_get(native_routines, "useRegistration");
    if use_registration.kind() != SexpType::Logical
        || length(use_registration) != 1
        || values::ints(use_registration)[0] == NA_LOGICAL
        || length(values::list_get(native_routines, "symbolNames")) != 0
    {
        return decline();
    }
    let mut variables: Vec<(Sexp, Sexp)> = Vec::new();
    let mut routine_names: Vec<Sexp> = Vec::new();
    if values::ints(use_registration)[0] != 0 {
        let fixes = values::list_get(native_routines, "registrationFixes");
        if fixes.kind() != SexpType::String
            || length(fixes) != 2
            || (0..2).any(|index| {
                fixes.element(index) == values::na_string()
                    || !fixes.element(index).bytes().is_ascii()
            })
        {
            return decline();
        }
        let Some(dll) = argument(frame, install("dll")?)? else {
            return decline();
        };
        if dll.kind() != SexpType::List || !values::inherits(dll, "DLLInfo") {
            return decline();
        }
        let info = values::list_get(dll, "info");
        let Some(registered) = crate::builtins::internal_function(install("getRegisteredRoutines")?)
        else {
            return decline();
        };
        let routines = eval::call_builtin(
            eval::builtin_def(registered)?,
            nil(),
            registered,
            &[Arg { tag: nil(), value: info }],
            values::global_env(),
        )?;
        crate::heap().root(routines);
        let Some(env) = argument(frame, install("env")?)? else {
            return decline();
        };
        if env.kind() != SexpType::Environment || routines.kind() != SexpType::List {
            return decline();
        }
        let (prefix, suffix) = (fixes.element(0).bytes(), fixes.element(1).bytes());
        for type_index in 0..length(routines) {
            let routines_of_type = routines.element(type_index);
            if routines_of_type.kind() != SexpType::List {
                return decline();
            }
            for index in 0..length(routines_of_type) {
                let symbol_info = routines_of_type.element(index);
                let name = values::list_get(symbol_info, "name");
                if name.kind() != SexpType::String
                    || length(name) != 1
                    || name.element(0) == values::na_string()
                    || !name.element(0).bytes().is_ascii()
                {
                    return decline();
                }
                let mut variable = prefix.to_vec();
                variable.extend_from_slice(name.element(0).bytes());
                variable.extend_from_slice(suffix);
                let variable = values::install_bytes(&variable)?;
                if crate::heap().frame_cell(env, variable).is_some()
                    || variables.iter().any(|&(bound, _)| bound == variable)
                {
                    return decline();
                }
                variables.push((variable, symbol_info));
                routine_names.push(name.element(0));
            }
        }
        for &(variable, symbol_info) in &variables {
            eval::define(variable, symbol_info, env)?;
        }
    }
    let value = values::alloc(SexpType::String, routine_names.len())?;
    crate::heap().root(value);
    let names = values::alloc(SexpType::String, variables.len())?;
    crate::heap().root(names);
    for (index, (&name, &(variable, _))) in routine_names.iter().zip(&variables).enumerate() {
        crate::heap().set_element(value, index, name);
        crate::heap().set_element(names, index, variable.car());
    }
    values::set_attr(value, symbols().names, names)?;
    finish(frame, value, true)
}
