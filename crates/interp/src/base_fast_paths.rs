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
    heap::object::{NA_LOGICAL, Sexp, SexpType},
    smallvec::SmallVec,
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("ryMatchArg", 0, do_match_arg),
    internal("ryStopifnot", 0, do_stopifnot),
    internal("rySetdiff", 0, do_setdiff),
    internal("ryStructure", 0, do_structure),
    internal("ryFormatDefault", 0, do_format_default),
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
    Ok(())
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

/// Whether the formal `name` holds its own default, untouched: either no value at all or the
/// default's promise, which is evaluated in the frame itself. An argument the caller passed on
/// from its own missing formal is not a default, because forcing it raises an error.
unsafe fn is_default(frame: Sexp, name: &str) -> R<bool> {
    let Some(cell) = crate::heap().frame_cell(frame, install(name)?) else {
        return Ok(true);
    };
    let value = cell.cdr();
    Ok(value == missing_arg()
        || (value.kind() == SexpType::Promise && value.tag() == frame))
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
        let mut partial = (0..count).filter(|&index| {
            choices
                .element(index)
                .bytes()
                .starts_with(target.bytes())
        });
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
    let existing = kernel(crate::subset::do_attributes, &[data], frame)?;
    let existing_names = values::names(existing);
    let mut combined: Vec<(String, Sexp)> = Vec::with_capacity(length(existing) + attributes.len());
    for index in 0..length(existing) {
        let name = values::string_at(existing_names, index).unwrap_or_default();
        combined.push((name.into_owned(), existing.element(index)));
    }
    for &(tag, value) in &attributes {
        combined.push((values::symbol_name(tag).into_owned(), value));
    }
    let pairs: Vec<(&str, Sexp)> = combined
        .iter()
        .map(|(name, value)| (name.as_str(), *value))
        .collect();
    let list = values::named_list(&pairs)?;
    crate::heap().root(list);
    let result = kernel(crate::subset::do_set_attributes, &[data, list], frame)?;
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
