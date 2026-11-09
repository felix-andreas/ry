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
        session::{FNV_OFFSET, eval_text, stable_hash},
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
    internal("ryStructure", 0, do_structure),
    internal("ryFormatDefault", 0, do_format_default),
    internal("ryReduce", 0, do_reduce),
];

/// The base functions that get a fast path, with the internal their prologue calls and the
/// [`stable_hash`] of the formals and body each was made from (GNU R 4.6.2). Setting
/// `RY_PRINT_FAST_PATH_HASHES` prints the hashes of the loaded ones.
const FAST_PATHS: &[(&str, &str, u64)] = &[
    ("match.arg", "ryMatchArg", 0x0d569925082b596e),
    ("stopifnot", "ryStopifnot", 0x036d82929ad9613c),
    ("setdiff", "rySetdiff", 0x9f92f07db3152727),
    ("structure", "ryStructure", 0xe1de236049200229),
    ("format.default", "ryFormatDefault", 0x5eb64c51109fbc94),
    ("Reduce", "ryReduce", 0xd929e4c219983788),
];

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
        if function.kind() != SexpType::Closure {
            missed.push(name);
            continue;
        }
        let prologue = eval_text(&format!("quote(.Internal({internal_name}()))"), namespace)?;
        crate::heap().root(prologue);
        let mut body = crate::bytecode::closure_body(function);
        let mut already = false;
        if body.kind() == SexpType::Language
            && body.car() == install("{")?
            && values::pairlist_length(body.cdr()) == 2
            && crate::sort::identical(body.cdr().car(), prologue, &IdenticalFlags::default())
        {
            body = body.cdr().cdr().car();
            already = true;
        }
        let mut hash = FNV_OFFSET;
        stable_hash(function.car(), &mut hash);
        stable_hash(body, &mut hash);
        if print {
            eprintln!("{name} {hash:#018x}");
        }
        if hash != expected {
            missed.push(name);
            continue;
        }
        if !already {
            let block = values::lcons(
                install("{")?,
                values::cons(prologue, values::cons(body, nil())?)?,
            )?;
            crate::heap().set_cdr(function, block);
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
        return match crate::code_image::native_hooks() {
            Some(hooks) => (hooks.call)(function, args).transpose(),
            None => Ok(None),
        };
    };
    let value = entry(args)?;
    if value.is_some() {
        activity::count(Counter::NativeEntries);
        interp().visible = true;
    }
    Ok(value)
}

/// Runs the call of `function`, a closure marked [`gp::NATIVE_ENTRY`], natively from its
/// unevaluated `arguments` when each is a constant or a variable bound to a value, so the call
/// builds no promise at all. Reading such an argument runs no R code, so reading it now gives
/// the value forcing its promise would give, and the native code reads only values; when it
/// declines, nothing has changed and the caller builds the promises as usual.
pub unsafe fn native_entry_by_value(function: Sexp, arguments: Sexp, env: Sexp) -> R<Option<Sexp>> {
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
                match crate::effects::eager_argument(expression, env) {
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
    if argument.has_gp(gp::CHECKED_PROMISE) {
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
    values::scalar_string(crate::coerce::type_name(x)).map(Some)
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
    let which = match which {
        Some(which) => which,
        None => {
            let which = values::scalar_bool(false)?;
            crate::heap().root(which);
            which
        }
    };
    kernel(crate::dispatch::do_inherits, &[x, what, which], nil()).map(Some)
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
    kernel(crate::coerce::do_is_vector, &[x, mode], nil()).map(Some)
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
    let positions = kernel(sort::do_match, &[x, table, nomatch, nil()], nil())?;
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

/// The prologue's result when the R body is to run.
fn decline() -> R<Sexp> {
    Ok(nil())
}

/// Finishes the call of the closure whose frame is `frame` with `value`, as `return(value)`.
unsafe fn finish(frame: Sexp, value: Sexp, visible: bool) -> R<Sexp> {
    interp().visible = visible;
    Err(jump(Jump::Return { env: frame, value }))
}

/// The value of the formal `name` in `frame`, supplied or defaulted, forced as evaluating the
/// name would; `None` when it has neither a value nor a default.
unsafe fn argument(frame: Sexp, name: &str) -> R<Option<Sexp>> {
    let symbol = install(name)?;
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
unsafe fn is_default(frame: Sexp, name: &str) -> R<bool> {
    let Some(cell) = crate::heap().frame_cell(frame, install(name)?) else {
        return Ok(true);
    };
    Ok(cell.cdr() == missing_arg() || cell.has_gp(gp::BINDING_MISSING))
}

/// Whether the call supplied the formal `name`, as `missing(name)` is false.
unsafe fn is_supplied(frame: Sexp, name: &str) -> R<bool> {
    Ok(!eval::is_missing(install(name)?, frame)?)
}

/// The elements of the frame's `...`, unforced; `None` when one is an empty argument.
unsafe fn dots(frame: Sexp) -> R<Option<SmallVec<[(Sexp, Sexp); 8]>>> {
    let Some(cell) = crate::heap().frame_cell(frame, symbols().dots) else {
        return Ok(Some(SmallVec::new()));
    };
    let list = cell.cdr();
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

/// Calls the builtin `function` with positional `args`, for a kernel that reads only its
/// arguments.
unsafe fn kernel(function: BuiltinFn, args: &[Sexp], env: Sexp) -> R<Sexp> {
    let args: SmallVec<[Arg; 9]> = args
        .iter()
        .map(|&value| Arg { tag: nil(), value })
        .collect();
    let call = Call {
        call: nil(),
        op: nil(),
        args: &args,
        env,
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
    let frame = call.env;
    if !is_default(frame, "several.ok")? {
        return decline();
    }
    // The R body evaluates a missing `choices` before it forces `arg`, and a supplied one after.
    let defaulted = if is_supplied(frame, "choices")? {
        None
    } else {
        let Some(choices) = default_choices(frame)? else {
            return decline();
        };
        Some(choices)
    };
    let Some(arg) = argument(frame, "arg")? else {
        return decline();
    };
    if arg.kind() != SexpType::String || arg.attrib() != nil() {
        return decline();
    }
    let choices = match defaulted {
        Some(choices) => choices,
        None => match argument(frame, "choices")? {
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
        return finish(frame, result, true);
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
    finish(frame, result, true)
}

/// The default of the caller's formal that `arg` names, evaluated in the caller's frame, when
/// that default is a character constant or a `c()` of character constants. It binds `choices`
/// in the frame as the R body does, so a declined fast path does not evaluate it a second time.
unsafe fn default_choices(frame: Sexp) -> R<Option<Sexp>> {
    let Some(cell) = crate::heap().frame_cell(frame, install("arg")?) else {
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
        || (default.kind() == SexpType::Language && default.car() == install("c")? && {
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
    eval::define(install("choices")?, choices, frame)?;
    Ok(Some(choices))
}

/// `stopifnot(...)` when every expression is all `TRUE`. The first that is not leaves the R body
/// to build the error from its call.
unsafe fn do_stopifnot(call: &Call<'_>) -> R<Sexp> {
    let frame = call.env;
    if is_supplied(frame, "exprs")? || is_supplied(frame, "exprObject")? {
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

/// `Reduce(f, x, init, right)` without `accumulate`, over a plain vector and with `f` a closure
/// or a builtin. The R body calls `forceAndCall(2, f, init, x[[i]])` per element, which builds a
/// call and evaluates `x[[i]]` through the evaluator each time; this loop reads the element
/// directly and passes both arguments as forced promises of the same expressions, so `f` sees
/// the same call, the same `substitute()` results and the same frame variables `init` and `i`.
unsafe fn do_reduce(call: &Call<'_>) -> R<Sexp> {
    let frame = call.env;
    let missing_init = !is_supplied(frame, "init")?;
    let Some(x) = argument(frame, "x")? else {
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
    let Some(function) = argument(frame, "f")? else {
        return decline();
    };
    if !matches!(function.kind(), SexpType::Closure | SexpType::Builtin) {
        return decline();
    }
    // The R body reads `right` before `accumulate` when `init` is missing, and after otherwise.
    // A flag that is not TRUE or FALSE stops the R body with an error before it reads the
    // other one, so the prologue declines before forcing it.
    let (right, accumulate) = if missing_init {
        let Some(right) = flag(frame, "right")? else {
            return decline();
        };
        (right, flag(frame, "accumulate")?)
    } else {
        let Some(accumulate) = flag(frame, "accumulate")? else {
            return decline();
        };
        let Some(right) = flag(frame, "right")? else {
            return decline();
        };
        (right, Some(accumulate))
    };
    let Some(false) = accumulate else {
        return decline();
    };
    let symbols = symbols();
    let heap = crate::heap();
    let init_symbol = install("init")?;
    let i_symbol = install("i")?;
    eval::define(
        install("mis")?,
        values::scalar_logical(missing_init as i32)?,
        frame,
    )?;
    eval::define(
        install("len")?,
        values::scalar_integer(count as i32)?,
        frame,
    )?;
    eval::define(install("f")?, function, frame)?;
    let element_call = values::call(symbols.bracket2, &[install("x")?, i_symbol])?;
    heap.root(element_call);
    let arguments = if right {
        [element_call, init_symbol]
    } else {
        [init_symbol, element_call]
    };
    let inner_call = values::call(install("f")?, &arguments)?;
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
    } else if argument(frame, "init")?.is_none() {
        return decline();
    }
    let step = |position: usize| -> R<()> {
        let index = values::scalar_integer(position as i32 + 1)?;
        eval::define(i_symbol, index, frame)?;
        let element = crate::subset::subset2(x, &[index], Some(true), element_call)?;
        crate::heap().root(element);
        let init = eval::find_var_in_frame(init_symbol, frame)?;
        let init = eval::value_of(init)?;
        let values = if right {
            [element, init]
        } else {
            [init, element]
        };
        let value = eval::call_with_values(inner_call, function, &values, frame)?;
        eval::define(init_symbol, value, frame)
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
    let value = eval::value_of(eval::find_var_in_frame(init_symbol, frame)?)?;
    finish(frame, value, true)
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
unsafe fn flag(frame: Sexp, name: &str) -> R<Option<bool>> {
    let Some(value) = argument(frame, name)? else {
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
    let frame = call.env;
    let Some(x) = argument(frame, "x")? else {
        return decline();
    };
    if x == nil() {
        return finish(frame, nil(), true);
    }
    let Some(y) = argument(frame, "y")? else {
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

/// `structure(.Data, ...)` when every attribute is named and is either a plain one or a `class`
/// that is a character vector without `"factor"`.
unsafe fn do_structure(call: &Call<'_>) -> R<Sexp> {
    let frame = call.env;
    let Some(data) = argument(frame, ".Data")? else {
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
    let frame = call.env;
    for name in [
        "justify",
        "big.mark",
        "big.interval",
        "small.mark",
        "small.interval",
        "zero.print",
        "drop0trailing",
    ] {
        if !is_default(frame, name)? {
            return decline();
        }
    }
    match dots(frame)? {
        Some(items) if items.is_empty() => {}
        _ => return decline(),
    }
    let Some(x) = argument(frame, "x")? else {
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
    for name in ["trim", "digits", "nsmall", "width"] {
        let Some(value) = argument(frame, name)? else {
            return decline();
        };
        args.push(value);
    }
    let justify_none = values::scalar_integer(3)?;
    crate::heap().root(justify_none);
    args.push(justify_none);
    for name in ["na.encode", "scientific", "decimal.mark"] {
        let Some(value) = argument(frame, name)? else {
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
