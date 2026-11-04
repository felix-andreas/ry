//! The methods package's native core: class queries, object creation and validation.
//!
//! GNU R's methods package is R code over a few C routines, and its per-call cost is that R
//! code: `new()` runs about forty R calls before it returns. Each function this module speeds up
//! keeps its R body, and when the methods namespace loads, [`install_fast_paths`] puts a native fast path
//! (`methods_core.R`) in front of it. A fast path either does the function's whole work, with the
//! same value and the same effects, or returns without any observable effect and lets the R body
//! run. So it bails out wherever it would have to run R code someone could observe (a validity
//! method, a coerce method, a user's `initialize` method, a promise that is not a plain value),
//! raise a condition, or look a class up past the class cache. The R bodies follow one GNU R
//! release, so a fast path goes in front of a function only when that function is exactly the
//! code the fast path was made from, recognised by its hash.

use {
    crate::{
        builtins::{BuiltinDef, Call, internal},
        dispatch, eval, interp,
        session::{FNV_OFFSET, eval_text, stable_hash},
        unwind::R,
        values::{
            self, cons, cons_tagged, get_attr, install, lcons, length, missing_arg, nil,
            scalar_integer, symbols, unbound,
        },
    },
    heap::object::{Sexp, SexpType, gp},
};

/// Returns `false` from a definition fast path so the R body runs. Setting
/// `RY_TRACE_METHODS_BAILS` prints the source line of every bail, which shows why a definition
/// takes the slow path.
macro_rules! bail {
    ($skip:expr) => {{
        if trace_bails() {
            eprintln!("methods fast path bails at methods_core.rs:{}", line!());
        }
        return Ok($skip);
    }};
}

fn trace_bails() -> bool {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("RY_TRACE_METHODS_BAILS").is_some())
}

pub const BUILTINS: &[BuiltinDef] = &[
    internal("ryS4New", 1, do_new),
    internal("ryS4Initialize", 1, do_initialize),
    internal("ryS4Valid", 2, do_valid),
    internal("ryS4Is", 2, do_is),
    internal("ryS4Extends", 1, do_extends),
    internal("ryS4SetGeneric", 1, do_set_generic),
    internal("ryS4SetMethod", 1, do_set_method),
    internal("ryS4SetClass", 1, do_set_class),
];

/// The methods package's functions that get a fast path, with the [`stable_hash`] of the formals
/// and body each was made from, one per supported version of the package (ry's bundled base R
/// and the GNU R versions `R_HOME` may name). `initialize` is the default method of the
/// `initialize` generic. Setting `RY_PRINT_METHODS_HASHES` prints the hashes of the loaded ones,
/// for updating this together with `methods_core.R` and the fast paths.
const FAST_PATHS: &[(&str, &[u64])] = &[
    ("new", &[0xed04_11d7_4c52_3b14]),
    ("initialize", &[0x0cbc_a8bc_242d_4840]),
    ("validObject", &[0xcfec_1557_eba8_fe90]),
    ("is", &[0xa975_8041_9461_37c4, 0x4826_09db_60cf_b7e5]),
    ("extends", &[0x2df8_66a4_f3b4_1f5e]),
    ("setGeneric", &[0xe3ac_b876_a896_9c54]),
    ("setMethod", &[0x7cc6_fd5c_68e7_5d79]),
    ("setClass", &[0x3a03_d885_5064_d505]),
];

/// The version of GNU R that ry's bundled base R is built from. Its methods package is the one
/// every fast path must match, so a fast path that does not install there is an error rather
/// than a silent loss of speed.
const BUNDLED_VERSION: &str = include_str!("../../../tools/base-r/VERSION");

/// Puts the fast paths of `methods_core.R` in front of the functions of the methods namespace
/// `namespace` they were made for, changing each closure in place so that every reference to it
/// gets the fast path. On the bundled version of the package every fast path must install.
pub unsafe fn install_fast_paths(namespace: Sexp) -> R<()> {
    let prologues = eval_text(include_str!("methods_core.R"), namespace)?;
    crate::heap().root(prologues);
    interp().globals.methods_templates = eval_text(METHODS_TEMPLATES, namespace)?;
    let version = eval_text(".__NAMESPACE__.$spec[[\"version\"]]", namespace)?;
    let bundled =
        values::first_string(version).is_some_and(|version| version == BUNDLED_VERSION.trim());
    let names = values::names(prologues);
    let print = std::env::var_os("RY_PRINT_METHODS_HASHES").is_some();
    let mut missed = Vec::new();
    for index in 0..length(prologues) {
        let Some(name) = values::string_at(names, index) else {
            continue;
        };
        let closures = if name == "initialize" {
            default_initialize_methods(namespace)?
        } else {
            let value = eval::find_var_in_frame(install(&name)?, namespace)?;
            vec![eval::value_of(value)?]
        };
        let expected = FAST_PATHS
            .iter()
            .find(|(known, _)| *known == name)
            .map_or(&[][..], |&(_, hashes)| hashes);
        let prologue = prologues.element(index);
        let mut prologue_hash = FNV_OFFSET;
        stable_hash(prologue, &mut prologue_hash);
        let (mut seen, mut installed) = (false, false);
        for closure in closures {
            if closure.kind() != SexpType::Closure {
                continue;
            }
            seen = true;
            let mut body = crate::bytecode::closure_body(closure);
            // ry builds its bundled base R itself, so the methods namespace it saved holds the
            // closures with their fast paths already in front: hash the original body behind it.
            let mut already = false;
            if let Some([(_, first), (_, original)]) = call_args::<2>(body, "{")? {
                let mut first_hash = FNV_OFFSET;
                stable_hash(first, &mut first_hash);
                if first_hash == prologue_hash {
                    body = original;
                    already = true;
                }
            }
            let mut hash = FNV_OFFSET;
            stable_hash(closure.car(), &mut hash);
            stable_hash(body, &mut hash);
            if print {
                eprintln!("{name} {hash:#018x}");
            }
            if !expected.contains(&hash) {
                continue;
            }
            if !already {
                let block = lcons(install("{")?, cons(prologue, cons(body, nil())?)?)?;
                crate::heap().set_cdr(closure, block);
            }
            installed = true;
        }
        // The source build of the package calls this before it defines its generics, so
        // `initialize` may have no methods yet; that is not a miss.
        if seen && !installed {
            missed.push(name);
        }
    }
    if bundled && !missed.is_empty() {
        return Err(crate::unwind::error(format!(
            "the native fast paths for {} do not match ry's bundled methods package; update \
             FAST_PATHS in methods_core.rs (RY_PRINT_METHODS_HASHES=1 prints the hashes)",
            missed.join(", ")
        )));
    }
    Ok(())
}

/// Every method of the `initialize` generic the namespace holds: the generic's `default` slot
/// and every entry of its methods tables. Inherited dispatch caches a copy of the default method
/// per class (with its own `target`), and the saved namespace already holds such copies, so the
/// caller picks the default method's copies out by their hash.
unsafe fn default_initialize_methods(namespace: Sexp) -> R<Vec<Sexp>> {
    let generic = eval::value_of(eval::find_var_in_frame(install("initialize")?, namespace)?)?;
    if generic.kind() != SexpType::Closure {
        return Ok(Vec::new());
    }
    let mut methods = vec![get_attr(generic, install("default")?)];
    let table = eval::find_var_in_frame(install(".__T__initialize:methods")?, namespace)?;
    let mut tables = vec![eval::value_of(table)?];
    for name in [".MTable", ".AllMTable"] {
        tables.push(eval::value_of(eval::find_var_in_frame(
            install(name)?,
            generic.tag(),
        )?)?);
    }
    for table in tables {
        if table.kind() != SexpType::Environment {
            continue;
        }
        for symbol in crate::heap().frame_symbols(table) {
            let Some(method) = quiet_value(symbol, table)? else {
                continue;
            };
            if !methods.contains(&method) {
                methods.push(method);
            }
        }
    }
    Ok(methods)
}

// The fast paths. A frame is the environment of the R function whose body the fast path
// stands in front of.

/// `new(Class, ...)`: binds `ClassDef` and `value` in the frame of `new`, as its body would,
/// when the class cache has the class, so that `new` goes on to call `initialize(value, ...)`.
unsafe fn do_new(call: &Call<'_>) -> R<Sexp> {
    let frame = call.arg(0);
    let Some(class) = quiet_value(install("Class")?, frame)? else {
        return values::scalar_bool(false);
    };
    if class.kind() != SexpType::String && !class.has_gp(gp::S4_OBJECT) {
        return values::scalar_bool(false);
    }
    let Some(definition) = cached_class(class)? else {
        return values::scalar_bool(false);
    };
    if values::as_logical_scalar(slot(definition, "virtual")?) != 0 {
        return values::scalar_bool(false);
    }
    let value = crate::methods::new_object(definition)?;
    eval::define(install("ClassDef")?, definition, frame)?;
    eval::define(install("value")?, value, frame)?;
    values::scalar_bool(true)
}

/// The default `initialize` method. Unnamed arguments are taken last first: one of the object's
/// own class replaces it, and one of the class's data part goes through the data-part replacement
/// method `as<-` finds cached for it. Named arguments then set slots on a copy, and the method
/// binds the result as `.Object` and calls `validObject(.Object)` from its own frame, as the R code
/// does.
unsafe fn do_initialize(call: &Call<'_>) -> R<Sexp> {
    let frame = call.arg(0);
    let done = values::scalar_bool(true)?;
    let skip = values::scalar_bool(false)?;
    let object_symbol = install(".Object")?;
    let Some(mut object) = quiet_value(object_symbol, frame)? else {
        bail!(skip);
    };
    let list_dots = lcons(install("list")?, cons(symbols().dots, nil())?)?;
    let args = eval::eval(list_dots, frame)?;
    crate::heap().root(args);
    if length(args) == 0 {
        return Ok(done);
    }
    let class = dispatch::class_of(object)?;
    if length(class) != 1 || basic_classes()?.contains(&class.element(0).bytes()) {
        bail!(skip);
    }
    let Some(definition) = cached_class(class)? else {
        bail!(skip);
    };
    let names = values::names(args);
    let name_at = |index: usize| -> &[u8] {
        if names.kind() == SexpType::String {
            names.element(index).bytes()
        } else {
            b""
        }
    };
    let slots = slot(definition, "slots")?;
    if slots.kind() != SexpType::List {
        bail!(skip);
    }
    let slot_names = values::names(slots);
    let package = slot(definition, "package")?;
    let class_name = class.element(0).bytes();
    let data_class = element(slots, b".Data");

    let mut supers = Vec::new();
    for index in (0..length(args)).rev() {
        if !name_at(index).is_empty() {
            continue;
        }
        let value = args.element(index);
        let value_class = dispatch::class_of(value)?;
        if value_class.element(0).bytes() == class_name {
            supers.push((value, false));
            continue;
        }
        if !data_part_super(class, definition, value_class, data_class)? {
            bail!(skip);
        }
        supers.push((value, true));
    }

    let mut assignments = Vec::with_capacity(length(args));
    for index in 0..length(args) {
        let name_bytes = name_at(index);
        if name_bytes.is_empty() {
            continue;
        }
        if (0..index).any(|earlier| name_at(earlier) == name_bytes) {
            bail!(skip);
        }
        let Some(position) = position_of(slot_names, name_bytes) else {
            bail!(skip);
        };
        let slot_class = slots.element(position);
        if slot_class.kind() != SexpType::String || length(slot_class) != 1 {
            bail!(skip);
        }
        let value = args.element(index);
        let value_class = dispatch::class_of(value)?;
        let same = length(value_class) == 1
            && value_class.element(0).bytes() == slot_class.element(0).bytes();
        // The R code replaces `.Object` with what the first slot assignment returns and drops
        // what later ones return, relying on `slot<-` modifying its argument. A data part is set
        // on a new object, so it is kept only as the first named argument.
        if name_bytes == b".Data" && (!same || !assignments.is_empty()) {
            bail!(skip);
        }
        if !same {
            let Some(slot_definition) = cached_class(slot_class)? else {
                bail!(skip);
            };
            if slot_class.element(0).bytes() == b"ANY" {
                // `as(value, "ANY")` is the value itself; only the R code's lookup of the
                // value's class is left, which has no effect once the package is loaded.
                if cached_class(value_class)?.is_none() && !package_loaded(package)? {
                    bail!(skip);
                }
            } else {
                let Some(value_definition) = cached_class(value_class)? else {
                    bail!(skip);
                };
                let relation = possible_extends(
                    value_class,
                    slot_class.element(0).bytes(),
                    Some(value_definition),
                    Some(slot_definition),
                )?;
                // A value of a class that extends the slot's goes through `as()`, which caches a
                // coerce method the first time; once that method is the identity, the value is
                // assigned as it is.
                if !matches!(relation, Some(Relation::No))
                    && !identity_coerce(
                        value_class.element(0).bytes(),
                        slot_class.element(0).bytes(),
                    )?
                {
                    bail!(skip);
                }
            }
        }
        assignments.push((install_text(names.element(index))?, value));
    }

    for (value, data_part) in supers {
        object = if data_part {
            crate::subset::assign_slot(object, symbols().dot_data, value)?
        } else {
            value
        };
        crate::heap().root(object);
    }
    if !assignments.is_empty() {
        let mut copy = values::shallow_duplicate(object)?;
        crate::heap().root(copy);
        for (index, (symbol, value)) in assignments.into_iter().enumerate() {
            let assigned = crate::subset::assign_slot(copy, symbol, value)?;
            if index == 0 {
                copy = assigned;
                crate::heap().root(copy);
            }
        }
        object = copy;
    }
    eval::define(object_symbol, object, frame)?;
    let validate = lcons(install("validObject")?, cons(object_symbol, nil())?)?;
    eval::eval(validate, frame)?;
    Ok(done)
}

/// Whether the R code takes an unnamed argument of class `value_class` as the data part of an
/// object of `class`, through `as(.Object, Classi) <- obj`, and does so without running anything
/// observable: the class must extend the argument's (and not the reverse), the argument's class
/// must be the data part's, and `as<-` must find the standard data-part replacement method
/// already cached in `coerce<-`'s table.
unsafe fn data_part_super(
    class: Sexp,
    definition: Sexp,
    value_class: Sexp,
    data_class: Option<Sexp>,
) -> R<bool> {
    let Some(data_class) = data_class else {
        return Ok(false);
    };
    if length(value_class) != 1
        || data_class.kind() != SexpType::String
        || length(data_class) != 1
        || data_class.element(0).bytes() != value_class.element(0).bytes()
    {
        return Ok(false);
    }
    let super_class = value_class.element(0).bytes();
    let Some(value_definition) = cached_class(value_class)? else {
        return Ok(false);
    };
    if name_extends(
        value_class,
        Some(value_definition),
        class.element(0).bytes(),
    )? != Some(false)
        || name_extends(class, Some(definition), super_class)? != Some(true)
    {
        return Ok(false);
    }
    // `getGeneric()` finds the generic in the generic table; the namespace's own binding may be
    // a lazy-load promise no one has forced.
    let methods = interp().globals.methods_namespace;
    let Some(generics) = quiet_value(install(".genericTable")?, methods)? else {
        return Ok(false);
    };
    if generics.kind() != SexpType::Environment {
        return Ok(false);
    }
    let Some(generic) = quiet_value(install("coerce<-")?, generics)? else {
        return Ok(false);
    };
    if generic.kind() != SexpType::Closure {
        return Ok(false);
    }
    let Some(table) = quiet_value(install(".MTable")?, generic.tag())? else {
        return Ok(false);
    };
    if table.kind() != SexpType::Environment {
        return Ok(false);
    }
    let mut label = class.element(0).bytes().to_vec();
    label.push(b'#');
    label.extend_from_slice(super_class);
    let Some(method) = quiet_value(values::install_bytes(&label)?, table)? else {
        return Ok(false);
    };
    is_data_part_replacement(method, super_class)
}

/// Whether `method` is the replacement method the methods package makes for a data part of class
/// `data_class`: `function(from, to, value) { from@.Data <- as(value, data_class, strict =
/// FALSE); from }`. For a value of exactly that class, calling it sets the data part with
/// `setDataPart()` and nothing else.
unsafe fn is_data_part_replacement(method: Sexp, data_class: &[u8]) -> R<bool> {
    if method.kind() != SexpType::Closure {
        return Ok(false);
    }
    let (from, value) = (install("from")?, install("value")?);
    let mut formal = method.car();
    for expected in [from, install("to")?, value] {
        if formal.kind() != SexpType::Pairlist || formal.tag() != expected {
            return Ok(false);
        }
        formal = formal.cdr();
    }
    if formal != nil() {
        return Ok(false);
    }
    let Some([assignment, result]) = call_args(crate::bytecode::closure_body(method), "{")? else {
        return Ok(false);
    };
    if result.1 != from {
        return Ok(false);
    }
    let Some([target, coercion]) = call_args(assignment.1, "<-")? else {
        return Ok(false);
    };
    let Some([object, part]) = call_args(target.1, "@")? else {
        return Ok(false);
    };
    let Some([coerced, class, strict]) = call_args(coercion.1, "as")? else {
        return Ok(false);
    };
    Ok(object.1 == from
        && part.1 == symbols().dot_data
        && coerced.1 == value
        && class.0 == nil()
        && class.1.kind() == SexpType::String
        && length(class.1) == 1
        && class.1.element(0).bytes() == data_class
        && strict.0 == install("strict")?
        && strict.1.kind() == SexpType::Logical
        && length(strict.1) == 1
        && is_false(strict.1))
}

/// The tags and values of the arguments of a call to `function` with exactly `N` arguments.
unsafe fn call_args<const N: usize>(call: Sexp, function: &str) -> R<Option<[(Sexp, Sexp); N]>> {
    if call.kind() != SexpType::Language || call.car() != install(function)? {
        return Ok(None);
    }
    let mut args = [(nil(), nil()); N];
    let mut cell = call.cdr();
    for arg in &mut args {
        if cell.kind() != SexpType::Pairlist {
            return Ok(None);
        }
        *arg = (cell.tag(), cell.car());
        cell = cell.cdr();
    }
    Ok((cell == nil()).then_some(args))
}

/// `validObject(object)` for an object whose slots all hold values of their declared classes
/// and whose class and superclasses define no validity method: `TRUE` when it checked that.
unsafe fn do_valid(call: &Call<'_>) -> R<Sexp> {
    let (object, frame) = (call.arg(0), call.arg(1));
    let valid = quiet_value(install("complete")?, frame)?
        .is_some_and(|complete| complete.kind() == SexpType::Logical && is_false(complete))
        && plainly_valid(object)?;
    values::scalar_bool(valid)
}

unsafe fn plainly_valid(object: Sexp) -> R<bool> {
    let class = dispatch::class_of(object)?;
    let Some(definition) = cached_class(class)? else {
        return Ok(false);
    };
    // The R code looks up the class's package first, which loads it when it is not loaded.
    if !package_loaded(slot(definition, "package")?)? {
        return Ok(false);
    }
    let slots = slot(definition, "slots")?;
    if slots.kind() != SexpType::List {
        return Ok(false);
    }
    let slot_names = values::names(slots);
    for index in 0..length(slots) {
        let name = slot_names.element(index);
        if name.bytes() == b".S3Class" {
            return Ok(false);
        }
        let slot_class = slots.element(index);
        if slot_class.kind() != SexpType::String || length(slot_class) != 1 {
            return Ok(false);
        }
        let Some(slot_definition) = cached_class(slot_class)? else {
            return Ok(false);
        };
        let value_class = if name.bytes() == b".Data" {
            match data_part_class(object, slot_class.element(0).bytes())? {
                Some(class) => values::scalar_string(class)?,
                None => return Ok(false),
            }
        } else {
            let symbol = install_text(name)?;
            if !has_attribute(object, symbol) {
                return Ok(false);
            }
            dispatch::class_of(crate::subset::get_slot(object, symbol, nil())?)?
        };
        let class2 = slot_class.element(0).bytes();
        if value_class.element(0).bytes() == class2 || class2 == b"ANY" {
            continue;
        }
        let Some(value_definition) = cached_class(value_class)? else {
            return Ok(false);
        };
        let relation = possible_extends(
            value_class,
            class2,
            Some(value_definition),
            Some(slot_definition),
        )?;
        if matches!(relation, None | Some(Relation::No)) {
            return Ok(false);
        }
    }
    let contains = slot(definition, "contains")?;
    for index in (0..length(contains)).rev() {
        let extension = contains.element(index);
        if !is_true(slot(extension, "simple")?) {
            return Ok(false);
        }
        let Some(superclass) = cached_class(slot(extension, "superClass")?)? else {
            return Ok(false);
        };
        if values::is_function(slot(superclass, "validity")?) {
            return Ok(false);
        }
    }
    Ok(!values::is_function(slot(definition, "validity")?))
}

/// The class of `getDataPart(object)` when the data part is a basic class that `getDataPart`
/// strips every attribute from, so the class is the implicit class of the object's type.
unsafe fn data_part_class(object: Sexp, data_class: &[u8]) -> R<Option<&'static str>> {
    if matches!(data_class, b"matrix" | b"array" | b"ts") || !basic_classes()?.contains(&data_class)
    {
        return Ok(None);
    }
    Ok(match object.kind() {
        SexpType::Logical => Some("logical"),
        SexpType::Integer => Some("integer"),
        SexpType::Real => Some("numeric"),
        SexpType::Complex => Some("complex"),
        SexpType::String => Some("character"),
        SexpType::List => Some("list"),
        SexpType::Raw => Some("raw"),
        SexpType::Closure | SexpType::Builtin | SexpType::Special => Some("function"),
        _ => None,
    })
}

const BAIL: i32 = 0;

/// The code a `switch()` fast path returns: `1` for `FALSE`, `2` for `TRUE`, `0` to run the R
/// body.
unsafe fn answer(result: Option<bool>) -> R<Sexp> {
    scalar_integer(match result {
        None => BAIL,
        Some(false) => 1,
        Some(true) => 2,
    })
}

/// `is(object, class2)`.
unsafe fn do_is(call: &Call<'_>) -> R<Sexp> {
    let (object, frame) = (call.arg(0), call.arg(1));
    let Some(class2) = quiet_value(install("class2")?, frame)? else {
        return answer(None);
    };
    answer(is_instance(object, class2)?)
}

unsafe fn is_instance(object: Sexp, class2: Sexp) -> R<Option<bool>> {
    let class1 = dispatch::class_of(object)?;
    let mut s3_case = length(class1) > 1;
    let class1 = if s3_case {
        values::scalar_string_charsxp(class1.element(0))?
    } else {
        class1
    };
    let (class2_name, given_definition) = match class2.kind() {
        SexpType::String if length(class2) == 1 => (class2, None),
        SexpType::Object if class2.has_gp(gp::S4_OBJECT) => {
            let name = get_attr(class2, install("className")?);
            if name.kind() != SexpType::String || length(name) != 1 {
                return Ok(None);
            }
            (name, Some(class2))
        }
        _ => return Ok(None),
    };
    let Some(definition1) = cached_class(class1)? else {
        return Ok(None);
    };
    let definition2 = match given_definition {
        Some(definition) => definition,
        None => match cached_class(class2_name)? {
            Some(definition) => definition,
            None => return Ok(None),
        },
    };
    let name2 = class2_name.element(0).bytes();
    s3_case = s3_case || (values::class_attr(object) != nil() && !object.has_gp(gp::S4_OBJECT));
    if s3_case {
        let old_class = basic_classes()?.contains(&name2)
            || match class_extends(definition2, b"oldClass")? {
                Some(extends) => extends,
                None => return Ok(None),
            };
        if old_class {
            if object.has_gp(gp::S4_OBJECT) {
                return Ok(None);
            }
            let classes = dispatch::class_of(object)?;
            return Ok(Some(
                (0..length(classes)).any(|index| classes.element(index).bytes() == name2),
            ));
        }
    }
    let name1 = class1.element(0).bytes();
    if name1 == name2 || name2 == b"ANY" {
        return Ok(Some(true));
    }
    if let Some(extension) = element(slot(definition1, "contains")?, name2) {
        return Ok(simple(extension)?.then_some(true));
    }
    if !is_plain_representation(definition2) {
        match is_class_union(definition2)? {
            None => return Ok(None),
            Some(true) => {
                let subclasses = values::names(slot(definition2, "subclasses")?);
                let contained = values::names(slot(definition1, "contains")?);
                let found = position_of(subclasses, name1).is_some()
                    || (0..length(contained)).any(|index| {
                        position_of(subclasses, contained.element(index).bytes()).is_some()
                    });
                return Ok(Some(found));
            }
            Some(false) => {}
        }
    }
    match element(slot(definition2, "subclasses")?, name1) {
        None => Ok(Some(false)),
        Some(extension) => Ok(simple(extension)?.then_some(true)),
    }
}

/// `extends(class1, class2)` with `maybe = TRUE` and `fullInfo = FALSE`.
unsafe fn do_extends(call: &Call<'_>) -> R<Sexp> {
    let frame = call.arg(0);
    let Some(class1) = quiet_value(install("class1")?, frame)? else {
        return answer(None);
    };
    let (name1, definition1) = match class1.kind() {
        SexpType::String if length(class1) >= 1 => {
            let name = if length(class1) > 1 {
                values::scalar_string_charsxp(class1.element(0))?
            } else {
                class1
            };
            match cached_class(name)? {
                Some(definition) => (name, definition),
                None => return answer(None),
            }
        }
        SexpType::Object if class1.has_gp(gp::S4_OBJECT) => {
            let representation = values::scalar_string("classRepresentation")?;
            if is_instance(class1, representation)? != Some(true) {
                return answer(None);
            }
            (slot(class1, "className")?, class1)
        }
        _ => return answer(None),
    };
    let flags = (
        quiet_value(install("maybe")?, frame)?,
        quiet_value(install("fullInfo")?, frame)?,
        quiet_value(install("class2")?, frame)?,
    );
    let (Some(maybe), Some(full_info), Some(class2)) = flags else {
        return answer(None);
    };
    if !is_true(maybe) || !is_false(full_info) {
        return answer(None);
    }
    if class2.kind() != SexpType::String || length(class2) != 1 {
        return answer(None);
    }
    answer(name_extends(
        name1,
        Some(definition1),
        class2.element(0).bytes(),
    )?)
}

// Class relations.

/// What `possibleExtends()` returns.
enum Relation {
    No,
    Yes,
    /// Through an `SClassExtension` object, which may have a test or a coercion of its own.
    Via,
}

/// Whether `as()` has cached the coerce method from `from` to `to` that returns its argument,
/// which it does for a simple extension such as a class union's member.
unsafe fn identity_coerce(from: &[u8], to: &[u8]) -> R<bool> {
    let Some(coerce) = methods_value("coerce")? else {
        return Ok(false);
    };
    if coerce.kind() != SexpType::Closure {
        return Ok(false);
    }
    let Some(table) = quiet_value(install(".AllMTable")?, coerce.tag())? else {
        return Ok(false);
    };
    if table.kind() != SexpType::Environment {
        return Ok(false);
    }
    let mut label = from.to_vec();
    label.push(b'#');
    label.extend_from_slice(to);
    let Some(method) = quiet_value(values::install_bytes(&label)?, table)? else {
        return Ok(false);
    };
    Ok(method.kind() == SexpType::Closure
        && crate::bytecode::closure_body(method) == install("from")?)
}

/// `possibleExtends(class1, class2, definition1, definition2)`, where `None` stands for a `NULL`
/// definition. `None` when deciding needs R code.
unsafe fn possible_extends(
    class1: Sexp,
    class2: &[u8],
    definition1: Option<Sexp>,
    definition2: Option<Sexp>,
) -> R<Option<Relation>> {
    if class1.element(0).bytes() == class2 || class2 == b"ANY" {
        return Ok(Some(Relation::Yes));
    }
    let Some(definition1) = definition1 else {
        return Ok(Some(Relation::No));
    };
    let contains = slot(definition1, "contains")?;
    if element(contains, class2).is_some() {
        return Ok(Some(Relation::Via));
    }
    let Some(definition2) = definition2 else {
        return Ok(Some(Relation::No));
    };
    let subclasses = slot(definition2, "subclasses")?;
    if !is_plain_representation(definition2) {
        match is_class_union(definition2)? {
            None => return Ok(None),
            Some(true) => {
                let subclass_names = values::names(subclasses);
                let contained = values::names(contains);
                let found = (0..length(class1))
                    .map(|index| class1.element(index))
                    .chain((0..length(contained)).map(|index| contained.element(index)))
                    .any(|name| position_of(subclass_names, name.bytes()).is_some());
                return Ok(Some(if found { Relation::Yes } else { Relation::No }));
            }
            Some(false) => {}
        }
    }
    for index in 0..length(class1) {
        if element(subclasses, class1.element(index).bytes()).is_some() {
            return Ok(Some(Relation::Via));
        }
    }
    Ok(Some(Relation::No))
}

/// `extends(class1, class2)` with `maybe = TRUE`, for the class named by the string `class1`
/// whose definition the caller looked up.
unsafe fn name_extends(class1: Sexp, definition1: Option<Sexp>, class2: &[u8]) -> R<Option<bool>> {
    if class1.element(0).bytes() == class2 || class2 == b"ANY" {
        return Ok(Some(true));
    }
    if let Some(definition1) = definition1
        && element(slot(definition1, "contains")?, class2).is_some()
    {
        return Ok(Some(true));
    }
    let Some(definition2) = cached_class(values::scalar_string(&String::from_utf8_lossy(class2))?)?
    else {
        return Ok(None);
    };
    let relation = possible_extends(class1, class2, definition1, Some(definition2))?;
    Ok(relation.map(|relation| !matches!(relation, Relation::No)))
}

/// `extends(definition, class2)` for a class definition.
unsafe fn class_extends(definition: Sexp, class2: &[u8]) -> R<Option<bool>> {
    name_extends(slot(definition, "className")?, Some(definition), class2)
}

/// `isClassUnion(definition)`: whether the class of the definition extends
/// `ClassUnionRepresentation`.
unsafe fn is_class_union(definition: Sexp) -> R<Option<bool>> {
    let class = values::class_attr(definition);
    if length(class) != 1 {
        return Ok(None);
    }
    let Some(class_definition) = cached_class(class)? else {
        return Ok(None);
    };
    name_extends(class, Some(class_definition), b"ClassUnionRepresentation")
}

/// Whether the definition's class is `classRepresentation` itself, which `is()` and
/// `possibleExtends()` take as never being a class union.
unsafe fn is_plain_representation(definition: Sexp) -> bool {
    let class = values::class_attr(definition);
    length(class) == 1 && class.element(0).bytes() == b"classRepresentation"
}

/// Whether an `SClassExtension` is simple: no test or coercion of its own.
unsafe fn simple(extension: Sexp) -> R<bool> {
    Ok(is_true(slot(extension, "simple")?))
}

// Lookups.

/// `getClassDef(class)` when the class cache answers it: the definition of the class the
/// string `class` names (with its `package` attribute), or a class definition passed in.
/// `None` when the R code would look further, in the environments or packages it searches.
unsafe fn cached_class(class: Sexp) -> R<Option<Sexp>> {
    let namespace = interp().globals.methods_namespace;
    let table = eval::value_of(eval::find_var_in_frame(install(".classTable")?, namespace)?)?;
    if table.kind() != SexpType::Environment {
        return Ok(None);
    }
    let definition = crate::methods::cached_class_definition(class, table)?;
    // More than one package defines the class: the R code picks one by where it is called from.
    if definition == nil()
        || (definition.kind() == SexpType::List && !definition.has_gp(gp::S4_OBJECT))
    {
        return Ok(None);
    }
    Ok(Some(definition))
}

/// Whether `.requirePackage(package)` returns without loading anything: the package is loaded,
/// or it names the global environment or the methods package.
unsafe fn package_loaded(package: Sexp) -> R<bool> {
    if package.kind() != SexpType::String || length(package) != 1 {
        return Ok(false);
    }
    let name = package.element(0);
    if matches!(name.bytes(), b".GlobalEnv" | b"methods") {
        return Ok(true);
    }
    if name.bytes().is_empty() {
        return Ok(false);
    }
    let registry = crate::heap().known().namespace_registry;
    Ok(crate::heap()
        .frame_cell(registry, install_text(name)?)
        .is_some())
}

/// The methods package's `.BasicClasses`.
unsafe fn basic_classes<'a>() -> R<Vec<&'a [u8]>> {
    let namespace = interp().globals.methods_namespace;
    let classes = eval::value_of(eval::find_var_in_frame(
        install(".BasicClasses")?,
        namespace,
    )?)?;
    if classes.kind() != SexpType::String {
        return Ok(Vec::new());
    }
    Ok((0..length(classes))
        .map(|index| classes.element(index).bytes())
        .collect())
}

/// The value bound to `symbol` in `frame` when getting it runs no code and cannot fail: a plain
/// value, a forced promise, or a promise of a constant or of a variable bound to such a value,
/// which is forced as the R code would force it. `None` for anything else, a missing argument
/// included.
unsafe fn quiet_value(symbol: Sexp, frame: Sexp) -> R<Option<Sexp>> {
    match crate::heap().frame_cell(frame, symbol) {
        Some(cell) => quiet_binding(cell, 0),
        None => Ok(None),
    }
}

unsafe fn quiet_binding(cell: Sexp, depth: u32) -> R<Option<Sexp>> {
    if cell.has_gp(gp::ACTIVE_BINDING) {
        return Ok(None);
    }
    let value = cell.cdr();
    if value == missing_arg() || value == unbound() {
        return Ok(None);
    }
    if value.kind() != SexpType::Promise {
        return Ok(Some(value));
    }
    if value.car() != unbound() {
        return Ok(Some(value.car()));
    }
    let code = value.cdr();
    let quiet = match code.kind() {
        SexpType::Language | SexpType::Promise | SexpType::Bytecode | SexpType::Dots => false,
        SexpType::Symbol => {
            depth < 4
                && code != symbols().dots
                && match eval::find_cell(code, value.tag()) {
                    Some(inner) => quiet_binding(inner, depth + 1)?.is_some(),
                    None => false,
                }
        }
        _ => true,
    };
    if !quiet {
        return Ok(None);
    }
    eval::force_promise(value).map(Some)
}

// Helpers.

unsafe fn slot(object: Sexp, name: &str) -> R<Sexp> {
    crate::subset::get_slot(object, install(name)?, nil())
}

/// `list[[name]]` for a name, which matches exactly: `None` for `NULL`.
unsafe fn element(list: Sexp, name: &[u8]) -> Option<Sexp> {
    if list.kind() != SexpType::List {
        return None;
    }
    let index = position_of(values::names(list), name)?;
    let value = list.element(index);
    (value != nil()).then_some(value)
}

unsafe fn position_of(strings: Sexp, name: &[u8]) -> Option<usize> {
    if strings.kind() != SexpType::String {
        return None;
    }
    (0..length(strings)).find(|&index| strings.element(index).bytes() == name)
}

unsafe fn has_attribute(object: Sexp, symbol: Sexp) -> bool {
    let mut cell = object.attrib();
    while cell.kind() == SexpType::Pairlist {
        if cell.tag() == symbol {
            return true;
        }
        cell = cell.cdr();
    }
    false
}

unsafe fn install_text(name: Sexp) -> R<Sexp> {
    values::install_charsxp(name)
}

unsafe fn is_true(value: Sexp) -> bool {
    value.kind() == SexpType::Logical && length(value) == 1 && values::ints(value)[0] == 1
}

unsafe fn is_false(value: Sexp) -> bool {
    value.kind() == SexpType::Logical && length(value) == 1 && values::ints(value)[0] == 0
}

// Definitions. `setGeneric`, `setMethod` and `setClass` run once per definition when a package
// loads, and their R code makes hundreds of calls each. Their fast paths take the shapes package
// code uses (a new standard generic, a method with a plain signature for an existing standard
// generic) and build exactly the objects the R code builds. Each checks every condition the R
// code branches on before it changes anything, and bails out on any other shape.

/// `setGeneric(name, def)` for a new standard generic: `def` is a closure whose body is
/// `standardGeneric(name)`, no function called `name` is visible from `where`, and every other
/// argument is missing. Builds the generic `makeGeneric()` builds, with its methods tables, and
/// does what `.GenericAssign()` does: binds it in `where`, caches it in `.genericTable`, and
/// binds an empty copy of its methods table as the `.__T__` metadata.
unsafe fn do_set_generic(call: &Call<'_>) -> R<Sexp> {
    let frame = call.arg(0);
    let skip = values::scalar_bool(false)?;
    if !native_definitions() {
        bail!(skip);
    }
    let Some(name) = forced_arg(frame, "name")? else {
        bail!(skip);
    };
    if !is_plain_name(name) {
        bail!(skip);
    }
    let name_text = name.element(0);
    let methods = interp().globals.methods_namespace;
    if in_basic_funs(name_text.bytes())? {
        bail!(skip);
    }
    for argument in [
        "group",
        "valueClass",
        "package",
        "signature",
        "useAsDefault",
        "genericFunction",
        "simpleInheritanceOnly",
    ] {
        if !eval::is_missing(install(argument)?, frame)? {
            bail!(skip);
        }
    }
    if eval::is_missing(install("def")?, frame)? {
        bail!(skip);
    }
    let Some(place) = forced_arg(frame, "where")? else {
        bail!(skip);
    };
    if place.kind() != SexpType::Environment {
        bail!(skip);
    }
    let name_symbol = install_text(name_text)?;
    if visible_function(name_symbol, place)? != Visible::None {
        bail!(skip);
    }
    if crate::envir::is_namespace(place) {
        if standard_packages_define(name_symbol)? != Some(false) {
            bail!(skip);
        }
    } else if visible_function(name_symbol, methods)? != Visible::None {
        bail!(skip);
    }
    let Some(def) = forced_arg(frame, "def")? else {
        bail!(skip);
    };
    if def.kind() != SexpType::Closure
        || !is_standard_generic_call(crate::bytecode::closure_body(def), name_text.bytes())?
    {
        bail!(skip);
    }
    let Some(package) = package_name(place)? else {
        bail!(skip);
    };
    let table_symbol = table_meta_symbol(name_text.bytes(), package.element(0).bytes())?;
    if !assignable(place, name_symbol) || !assignable(place, table_symbol) {
        bail!(skip);
    }
    let Some(generics) = methods_value(".genericTable")? else {
        bail!(skip);
    };
    if generics.kind() != SexpType::Environment {
        bail!(skip);
    }
    match quiet_value(name_symbol, generics)? {
        None => {}
        Some(previous) if previous.kind() == SexpType::Closure => {
            let previous_package = get_attr(previous, install("package")?);
            if !(previous_package.kind() == SexpType::String
                && length(previous_package) == 1
                && previous_package.element(0) == package.element(0))
            {
                bail!(skip);
            }
        }
        Some(_) => return Ok(skip),
    }
    let formal_names = formal_names(def.car());
    if formal_names.is_empty() {
        bail!(skip);
    }
    let mut signature: Vec<Sexp> = formal_names.clone();
    if signature.len() > 1 {
        signature.retain(|&symbol| symbol != symbols().dots);
    }
    if signature == [symbols().dots] {
        bail!(skip);
    }

    let generic = make_generic(name_text, def, package, &formal_names, &signature)?;
    eval::define(name_symbol, generic, place)?;
    eval::define(name_symbol, generic, generics)?;
    let table = hashed_env(generic.tag())?;
    eval::define(table_symbol, table, place)?;
    values::scalar_bool(true)
}

/// The generic function `makeGeneric(name, def, NULL, package = package)` makes from `def`, with
/// the tables `.setupMethodsTables()` puts in its environment.
unsafe fn make_generic(
    name_text: Sexp,
    def: Sexp,
    package: Sexp,
    formal_names: &[Sexp],
    signature: &[Sexp],
) -> R<Sexp> {
    let heap = crate::heap();
    let package_symbol = install("package")?;
    let generic_name = values::scalar_string_charsxp(name_text)?;
    values::set_attr(generic_name, package_symbol, package)?;
    let env = hashed_env(def.tag())?;
    let generic = values::triple(SexpType::Closure, def.car(), def.cdr(), env)?;
    eval::define(install(".Generic")?, generic_name, env)?;
    eval::define(install(".Methods")?, nil(), env)?;
    eval::define(install(".SigLength")?, values::scalar_real(1.0)?, env)?;
    let signature_symbols = values::list_vector(signature)?;
    eval::define(install(".SigArgs")?, signature_symbols, env)?;
    let methods_table = hashed_env(env)?;
    eval::define(install(".MTable")?, methods_table, env)?;
    let all_table = hashed_env(env)?;
    eval::define(install(".AllMTable")?, all_table, env)?;

    let signature_names = alloc_strings(signature.len())?;
    for (index, symbol) in signature.iter().enumerate() {
        heap.set_element(signature_names, index, symbol.car());
    }
    let skeleton = generic_skeleton(generic_name, def.car(), formal_names)?;
    let attributes: [(&str, Sexp); 8] = [
        ("generic", generic_name),
        ("package", package),
        ("group", values::alloc(SexpType::List, 0)?),
        ("valueClass", alloc_strings(0)?),
        ("signature", signature_names),
        ("default", s4_null()?),
        ("skeleton", skeleton),
        ("class", methods_class("standardGeneric")?),
    ];
    for (attribute, value) in attributes {
        values::set_attr(generic, install(attribute)?, value)?;
    }
    append_attributes(def, generic)?;
    generic.set_gp_bits(gp::S4_OBJECT, true);
    Ok(generic)
}

/// `generic.skeleton(name, def, NULL)`: a call of a function with the generic's formals that
/// reports the missing default method, on the formals as symbols. Arguments after `...` are
/// passed by name.
unsafe fn generic_skeleton(generic_name: Sexp, formals: Sexp, formal_names: &[Sexp]) -> R<Sexp> {
    let message =
        values::scalar_string("invalid call in method dispatch to '%s' (no default method)")?;
    let format = lcons(
        install("gettextf")?,
        cons(message, cons(generic_name, nil())?)?,
    )?;
    let domain = values::cons_tagged(
        values::scalar_logical(heap::object::NA_LOGICAL)?,
        nil(),
        install("domain")?,
    )?;
    let body = lcons(install("stop")?, cons(format, domain)?)?;
    let fallback = values::triple(SexpType::Closure, formals, body, values::base_env())?;
    let dots = formal_names
        .iter()
        .position(|&symbol| symbol == symbols().dots);
    let mut arguments = nil();
    for (index, &symbol) in formal_names.iter().enumerate().rev() {
        let tag = match dots {
            Some(dots) if dots + 1 < formal_names.len() && index > dots => symbol,
            _ => nil(),
        };
        arguments = values::cons_tagged(symbol, arguments, tag)?;
    }
    lcons(fallback, arguments)
}

/// `setMethod(f, signature, definition)` for a standard generic found from `where`, a closure
/// with the generic's formals, and a signature of class names the class cache defines. Does what
/// the R code does: caches the method in the generic's tables and in `where`'s `.__T__`
/// metadata, then resets the inherited methods, without the quadratic table rebuild.
unsafe fn do_set_method(call: &Call<'_>) -> R<Sexp> {
    let frame = call.arg(0);
    let skip = values::scalar_bool(false)?;
    if !native_definitions() {
        bail!(skip);
    }
    let Some(name) = forced_arg(frame, "f")? else {
        bail!(skip);
    };
    if !is_plain_name(name) || name.element(0).bytes() == b"as.double" {
        bail!(skip);
    }
    let Some(place) = forced_arg(frame, "where")? else {
        bail!(skip);
    };
    if place.kind() != SexpType::Environment || place.has_gp(gp::FRAME_LOCKED) {
        bail!(skip);
    }
    let name_symbol = install_text(name.element(0))?;
    let generic = crate::methods::visible_generic(name_symbol, place)?;
    if !is_standard_generic(generic)? {
        bail!(skip);
    }
    // `getGeneric()` caches a generic it finds that is also the global binding.
    if let Some(global) = quiet_value(name_symbol, values::global_env())? {
        if values::is_function(global) {
            let cached = methods_value(".genericTable")?
                .map(|table| quiet_value(name_symbol, table))
                .transpose()?
                .flatten();
            if global != generic || cached != Some(generic) {
                bail!(skip);
            }
        }
    }
    let table_symbol = table_meta_symbol(
        slot_text(generic, "generic")?,
        slot_text(generic, "package")?,
    )?;
    let sealed_check_table = match visible_function(name_symbol, place)? {
        Visible::None => None,
        Visible::Function(function) if function == generic => {
            match quiet_value(table_symbol, place)? {
                Some(table) if table.kind() == SexpType::Environment => Some(table),
                Some(_) => return Ok(skip),
                None => None,
            }
        }
        _ => return Ok(skip),
    };

    let Some(signature) = forced_arg(frame, "signature")? else {
        bail!(skip);
    };
    let Some(signature) = match_signature(signature, generic)? else {
        bail!(skip);
    };
    crate::heap().root(signature);
    let Some(definition) = forced_arg(frame, "definition")? else {
        bail!(skip);
    };
    if definition.kind() != SexpType::Closure
        || definition.has_gp(gp::S4_OBJECT)
        || values::class_attr(definition) != nil()
    {
        bail!(skip);
    }
    if !same_formals(definition.car(), generic.car()) {
        bail!(skip);
    }
    let env = generic.tag();
    let (Some(methods_table), Some(all_table)) = (
        quiet_value(install(".MTable")?, env)?,
        quiet_value(install(".AllMTable")?, env)?,
    ) else {
        bail!(skip);
    };
    if methods_table.kind() != SexpType::Environment || all_table.kind() != SexpType::Environment {
        bail!(skip);
    }
    let Some(signature_length) = quiet_value(install(".SigLength")?, env)?
        .and_then(|length| usize::try_from(values::as_integer_scalar(length)).ok())
    else {
        bail!(skip);
    };
    let Some(signature) = match_signature_length(signature, signature_length)? else {
        bail!(skip);
    };
    crate::heap().root(signature);
    let label = signature_label(signature)?;
    if let Some(table) = sealed_check_table
        && let Some(existing) = quiet_value(label, table)?
        && (existing.kind() == SexpType::Environment
            || values::inherits(existing, "SealedMethodDefinition"))
    {
        bail!(skip);
    }
    if !no_methods_lists()? || !cache_on_assign(place)? || duplicate_classes()? {
        bail!(skip);
    }
    let meta_table = match quiet_value(table_symbol, place)? {
        Some(table) if table.kind() == SexpType::Environment => Some(table),
        Some(_) => return Ok(skip),
        None => None,
    };
    // `.resetInheritedMethods()` turns a `MethodWithNext` it keeps back into a plain method
    // with `as()`, which runs R code.
    let direct = with_label(crate::heap().frame_symbols(methods_table), label);
    let all = with_label(crate::heap().frame_symbols(all_table), label);
    let copy_all = direct.iter().any(|symbol| !all.contains(symbol));
    for &symbol in &direct {
        if symbol == label {
            continue;
        }
        let source = if copy_all { methods_table } else { all_table };
        match quiet_value(symbol, source)? {
            Some(method) if !values::inherits(method, "MethodWithNext") => {}
            _ => return Ok(skip),
        }
    }

    let method = method_definition(definition, signature, generic)?;
    eval::define(label, method, methods_table)?;
    eval::define(label, method, all_table)?;
    let meta_table = match meta_table {
        Some(table) => table,
        None => {
            let table = hashed_env(env)?;
            eval::define(table_symbol, table, place)?;
            table
        }
    };
    eval::define(label, method, meta_table)?;
    reset_inherited_methods(methods_table, all_table)?;
    values::scalar_bool(true)
}

/// `.resetInheritedMethods(fenv, mtable)` once no kept method is a `MethodWithNext`: drops the
/// inherited methods from `.AllMTable`, and copies the direct ones in again when one is missing.
unsafe fn reset_inherited_methods(methods_table: Sexp, all_table: Sexp) -> R<()> {
    let heap = crate::heap();
    let direct = heap.frame_symbols(methods_table);
    let all = heap.frame_symbols(all_table);
    for &symbol in &all {
        if !direct.contains(&symbol) {
            heap.remove_binding(all_table, symbol)?;
        }
    }
    if direct.iter().any(|symbol| !all.contains(symbol)) {
        for &symbol in &direct {
            let value = eval::find_var_in_frame(symbol, methods_table)?;
            eval::define(symbol, value, all_table)?;
        }
    }
    Ok(())
}

/// The `MethodDefinition` that `asMethodDefinition(definition, signature)` makes, with its
/// `generic` slot set: a copy of the closure with the signature as its `target` and `defined`
/// slots.
unsafe fn method_definition(definition: Sexp, signature: Sexp, generic: Sexp) -> R<Sexp> {
    let method = values::triple(
        SexpType::Closure,
        definition.car(),
        definition.cdr(),
        definition.tag(),
    )?;
    let target = signature_object(signature, slot(generic, "signature")?)?;
    for (attribute, value) in [
        ("target", target),
        ("defined", target),
        ("generic", get_attr(generic, install("generic")?)),
        ("class", methods_class("MethodDefinition")?),
    ] {
        values::set_attr(method, install(attribute)?, value)?;
    }
    append_attributes(definition, method)?;
    method.set_gp_bits(gp::S4_OBJECT, true);
    Ok(method)
}

/// A `signature` object for a matched signature: its class names, named by the generic's
/// signature arguments `arguments` (which `.MakeSignature()` names them by when padding dropped
/// the names `matchSignature()` gave, the same ones), with their packages.
unsafe fn signature_object(signature: Sexp, arguments: Sexp) -> R<Sexp> {
    let count = length(signature);
    let object = alloc_strings(count)?;
    let names = alloc_strings(count)?;
    for index in 0..count {
        crate::heap().set_element(object, index, signature.element(index));
        let name = if index < length(arguments) {
            arguments.element(index)
        } else {
            values::na_string()
        };
        crate::heap().set_element(names, index, name);
    }
    let package_symbol = install("package")?;
    values::set_attr(object, symbols().names, names)?;
    values::set_attr(object, package_symbol, get_attr(signature, package_symbol))?;
    values::set_attr(object, symbols().class, methods_class("signature")?)?;
    object.set_gp_bits(gp::S4_OBJECT, true);
    Ok(object)
}

/// `matchSignature(signature, generic, where)` for an unnamed or exactly named character
/// signature: one class name per signature argument, `ANY` for the ones left out and trailing
/// ones dropped, named by the arguments, with the classes' packages as its `package`
/// attribute. Classes come from the class cache; `None` for anything else.
unsafe fn match_signature(signature: Sexp, generic: Sexp) -> R<Option<Sexp>> {
    let heap = crate::heap();
    let arguments = slot(generic, "signature")?;
    if arguments.kind() != SexpType::String {
        return Ok(None);
    }
    let count = length(signature);
    if signature.kind() != SexpType::String && !(signature == nil() || count == 0) {
        return Ok(None);
    }
    let package_symbol = install("package")?;
    let mut cell = signature.attrib();
    while cell.kind() == SexpType::Pairlist {
        if cell.tag() != symbols().names && cell.tag() != package_symbol {
            return Ok(None);
        }
        cell = cell.cdr();
    }
    if count == 0 {
        return Ok(Some(alloc_strings(0)?));
    }
    if count > length(arguments) {
        return Ok(None);
    }
    let given_packages = get_attr(signature, package_symbol);
    if given_packages != nil()
        && (given_packages.kind() != SexpType::String || length(given_packages) != count)
    {
        return Ok(None);
    }
    let mut classes = Vec::with_capacity(count);
    let mut packages = Vec::with_capacity(count);
    for index in 0..count {
        let class = signature.element(index);
        if class == values::na_string() {
            return Ok(None);
        }
        let given = if given_packages == nil() {
            values::blank_string()
        } else {
            given_packages.element(index)
        };
        let package = if given.bytes().is_empty() {
            let Some(definition) = cached_class(values::scalar_string_charsxp(class)?)? else {
                return Ok(None);
            };
            let package = slot(definition, "package")?;
            if package.kind() != SexpType::String || length(package) != 1 {
                return Ok(None);
            }
            package.element(0)
        } else {
            given
        };
        classes.push(class);
        packages.push(package);
    }
    // Named elements go to the arguments of the same name, unnamed ones to the rest in order,
    // as `match.call()` matches them; a partial or unknown name is left to the R code.
    let argument_count = length(arguments);
    let mut positions = vec![usize::MAX; count];
    let names = values::names(signature);
    let mut taken = vec![false; argument_count];
    if names != nil() {
        for index in 0..count {
            let name = names.element(index).bytes();
            if name.is_empty() {
                continue;
            }
            let Some(position) = position_of(arguments, name) else {
                return Ok(None);
            };
            if taken[position] {
                return Ok(None);
            }
            taken[position] = true;
            positions[index] = position;
        }
    }
    let mut next = 0;
    for position in &mut positions {
        if *position != usize::MAX {
            continue;
        }
        while next < argument_count && taken[next] {
            next += 1;
        }
        if next == argument_count {
            return Ok(None);
        }
        taken[next] = true;
        *position = next;
    }
    let any = values::mkchar("ANY")?;
    let methods = values::mkchar("methods")?;
    let mut value = vec![any; argument_count];
    let mut value_packages = vec![methods; argument_count];
    for index in 0..count {
        value[positions[index]] = classes[index];
        value_packages[positions[index]] = packages[index];
    }
    let mut kept = argument_count;
    while kept > 1 && value[kept - 1].bytes() == b"ANY" {
        kept -= 1;
    }
    let result = alloc_strings(kept)?;
    let result_names = alloc_strings(kept)?;
    let result_packages = alloc_strings(kept)?;
    for index in 0..kept {
        heap.set_element(result, index, value[index]);
        heap.set_element(result_names, index, arguments.element(index));
        heap.set_element(result_packages, index, value_packages[index]);
    }
    values::set_attr(result, symbols().names, result_names)?;
    values::set_attr(result, package_symbol, result_packages)?;
    Ok(Some(result))
}

/// `.matchSigLength(signature, ...)` for a generic that dispatches on `signature_length`
/// arguments: pads with `ANY` or drops trailing `ANY`s. `None` when the signature is longer,
/// which makes the R code rebuild the generic's tables.
unsafe fn match_signature_length(signature: Sexp, signature_length: usize) -> R<Option<Sexp>> {
    let count = length(signature);
    let mut kept = count;
    while kept > signature_length && signature.element(kept - 1).bytes() == b"ANY" {
        kept -= 1;
    }
    if kept > signature_length {
        return Ok(None);
    }
    let package_symbol = install("package")?;
    let packages = get_attr(signature, package_symbol);
    let size = signature_length.max(kept);
    let result = alloc_strings(size)?;
    let result_packages = alloc_strings(size)?;
    let any = values::mkchar("ANY")?;
    let methods = values::mkchar("methods")?;
    for index in 0..size {
        let (class, package) = if index < kept {
            (signature.element(index), packages.element(index))
        } else {
            (any, methods)
        };
        crate::heap().set_element(result, index, class);
        crate::heap().set_element(result_packages, index, package);
    }
    values::set_attr(result, package_symbol, result_packages)?;
    Ok(Some(result))
}

/// `setClass(Class, ...)` for a new class of the package `where` belongs to, given by `slots`
/// (or named `representation` entries) of defined classes, a `prototype` list of slot values,
/// and `contains` naming classes of the same package, or as `representation = "VIRTUAL"` alone.
/// Builds the definition `makeClassRepresentation()`, `completeClassDefinition()` and `setIs()`
/// build, with the same extension objects and their coerce and replace methods, and makes the
/// same changes: binds the definitions of the class and of each class it extends, which records
/// the new subclass, in `where` and in the class cache, and returns the generator function.
///
/// The shapes taken are the ones in which the R code computes the inheritance graph by
/// appending: the first superclass may have superclasses of its own, later ones may not, and no
/// class is reached twice. Every class involved is defined by the package itself, so its
/// definition is rebound where it lives.
unsafe fn do_set_class(call: &Call<'_>) -> R<Sexp> {
    let frame = call.arg(0);
    let skip = values::scalar_bool(false)?;
    if !native_definitions() {
        bail!(skip);
    }
    for name in ["access", "version", "sealed", "package", "S3methods"] {
        if !eval::is_missing(install(name)?, frame)? {
            bail!(skip);
        }
    }
    let Some(class) = forced_arg(frame, "Class")? else {
        bail!(skip);
    };
    if !is_plain_name(class) {
        bail!(skip);
    }
    let Some(place) = forced_arg(frame, "where")? else {
        bail!(skip);
    };
    if place.kind() != SexpType::Environment || place.has_gp(gp::FRAME_LOCKED) {
        bail!(skip);
    }
    let Some(package) = package_name(place)? else {
        bail!(skip);
    };
    // `.requirePackage(package)`, the environment of the coerce and replace methods.
    let registry = crate::heap().known().namespace_registry;
    if quiet_value(install_text(package.element(0))?, registry)? != Some(place) {
        bail!(skip);
    }
    if !cache_on_assign(place)? || duplicate_classes()? {
        bail!(skip);
    }
    let Some(class_table) = methods_value(".classTable")? else {
        bail!(skip);
    };
    if class_table.kind() != SexpType::Environment
        || crate::methods::cached_class_definition(class, class_table)? != nil()
    {
        bail!(skip);
    }
    let name = class.element(0);
    let meta = class_meta_symbol(name.bytes())?;
    if bound_from(meta, place) || !assignable(place, meta) {
        bail!(skip);
    }

    let mut own: Vec<(Sexp, Sexp)> = Vec::new();
    let mut virtual_class = false;
    if !eval::is_missing(install("slots")?, frame)? {
        if !eval::is_missing(install("representation")?, frame)? {
            bail!(skip);
        }
        let Some(slots) = forced_arg(frame, "slots")? else {
            bail!(skip);
        };
        let Some(pairs) = named_classes(slots) else {
            bail!(skip);
        };
        own = pairs;
    } else if !eval::is_missing(install("representation")?, frame)? {
        let Some(representation) = forced_arg(frame, "representation")? else {
            bail!(skip);
        };
        if is_plain_name(representation) && representation.element(0).bytes() == b"VIRTUAL" {
            virtual_class = true;
        } else {
            let Some(pairs) = named_classes(representation) else {
                bail!(skip);
            };
            own = pairs;
        }
    }
    let contains = if eval::is_missing(install("contains")?, frame)? {
        alloc_strings(0)?
    } else {
        match forced_arg(frame, "contains")? {
            Some(contains) => contains,
            None => bail!(skip),
        }
    };
    if contains.kind() != SexpType::String || contains.attrib() != nil() {
        bail!(skip);
    }
    let prototype = if eval::is_missing(install("prototype")?, frame)? {
        None
    } else {
        match forced_arg(frame, "prototype")? {
            Some(value) if value == nil() => None,
            Some(value) => Some(value),
            None => bail!(skip),
        }
    };
    // `.makeValidityMethod()` keeps a function whose one argument is `object` as it is.
    let validity = if eval::is_missing(install("validity")?, frame)? {
        nil()
    } else {
        match forced_arg(frame, "validity")? {
            Some(value) if value == nil() => value,
            Some(value)
                if value.kind() == SexpType::Closure
                    && value.car().kind() == SexpType::Pairlist
                    && value.car().cdr() == nil()
                    && value.car().tag() == install("object")? =>
            {
                value
            }
            _ => bail!(skip),
        }
    };
    let prototype_names = match prototype {
        None => Vec::new(),
        Some(list) => {
            let names = values::names(list);
            if list.kind() != SexpType::List
                || list.has_gp(gp::S4_OBJECT)
                || names.kind() != SexpType::String
                || list.attrib().cdr() != nil()
            {
                bail!(skip);
            }
            let names: Vec<Sexp> = (0..length(list))
                .map(|index| names.element(index))
                .collect();
            if !distinct_names(&names) {
                bail!(skip);
            }
            names
        }
    };
    if virtual_class && (length(contains) > 0 || prototype.is_some()) {
        bail!(skip);
    }
    if !virtual_class && own.is_empty() && length(contains) == 0 {
        bail!(skip);
    }

    let mut supers: Vec<Super> = Vec::new();
    for index in 0..length(contains) {
        let super_name = contains.element(index);
        if super_name == values::na_string()
            || supers
                .iter()
                .any(|known| known.name.bytes() == super_name.bytes())
            || super_name.bytes() == name.bytes()
        {
            bail!(skip);
        }
        let Some(definition) = local_class(super_name, package, place)? else {
            bail!(skip);
        };
        let superclasses = slot(definition, "contains")?;
        if index > 0 && length(superclasses) > 0 {
            bail!(skip);
        }
        supers.push(Super {
            name: super_name,
            definition,
        });
    }
    // The superclasses of the first superclass, which the new class extends through it.
    let mut ancestors: Vec<Super> = Vec::new();
    if let Some(first) = supers.first() {
        let extensions = slot(first.definition, "contains")?;
        let extension_names = values::names(extensions);
        for index in 0..length(extensions) {
            let ancestor_name = extension_names.element(index);
            if supers
                .iter()
                .any(|known| known.name.bytes() == ancestor_name.bytes())
            {
                bail!(skip);
            }
            let Some(definition) = local_class(ancestor_name, package, place)? else {
                bail!(skip);
            };
            if !plain_extension(extensions.element(index))? {
                bail!(skip);
            }
            ancestors.push(Super {
                name: ancestor_name,
                definition,
            });
        }
    }
    let mut lineage: Vec<Sexp> = supers.iter().map(|known| known.name).collect();
    lineage.extend(ancestors.iter().map(|known| known.name));
    for known in supers.iter().chain(&ancestors) {
        if element(slot(known.definition, "subclasses")?, name.bytes()).is_some() {
            bail!(skip);
        }
    }

    if let Some(needed) = methods_value(".NeedPrimitiveMethods")? {
        let needed = values::names(needed);
        if lineage
            .iter()
            .any(|known| position_of(needed, known.bytes()).is_some())
        {
            bail!(skip);
        }
    } else {
        bail!(skip);
    }
    if !definition_caches(name.bytes(), &lineage, prototype.is_some())? {
        bail!(skip);
    }

    // The slots: the class's own, then each superclass's, which include their superclasses'.
    let package_symbol = install("package")?;
    let mut slots: Vec<(Sexp, Sexp)> = Vec::with_capacity(own.len());
    for &(slot_name, slot_class) in &own {
        let Some(definition) = cached_class(values::scalar_string_charsxp(slot_class)?)? else {
            bail!(skip);
        };
        let qualified = values::scalar_string_charsxp(slot_class)?;
        values::set_attr(qualified, package_symbol, slot(definition, "package")?)?;
        slots.push((slot_name, qualified));
    }
    for known in &supers {
        let super_slots = slot(known.definition, "slots")?;
        let super_names = values::names(super_slots);
        for index in 0..length(super_slots) {
            let slot_name = super_names.element(index);
            if matches!(slot_name.bytes(), b".Data" | b".S3Class")
                || slots
                    .iter()
                    .any(|&(known, _)| known.bytes() == slot_name.bytes())
            {
                bail!(skip);
            }
            slots.push((slot_name, super_slots.element(index)));
        }
    }
    if prototype_names
        .iter()
        .any(|&prototype_name| !slots.iter().any(|&(known, _)| known == prototype_name))
    {
        bail!(skip);
    }

    let Some(default_prototype) = methods_value(".defaultPrototype")? else {
        bail!(skip);
    };
    let prototype_object = values::duplicate(default_prototype)?;
    if let Some(list) = prototype {
        for (index, &prototype_name) in prototype_names.iter().enumerate() {
            let value = list.element(index);
            let stored = if value == nil() { s4_null()? } else { value };
            values::set_attr(prototype_object, install_text(prototype_name)?, stored)?;
        }
    }
    // Without a prototype list, `reconcilePropertiesAndPrototype()` fills the missing own slots
    // before `defaultPrototype()` copies the superclasses' slots. With one, it turns the list
    // into a `classPrototypeDef` and fills them in as attributes of that wrapper instead, so the
    // object gets them only after the superclasses' slots.
    let own_first = prototype.is_none();
    for phase in [true, false] {
        if phase == own_first {
            for &(slot_name, slot_class) in &own {
                let symbol = install_text(slot_name)?;
                if has_attribute(prototype_object, symbol) {
                    continue;
                }
                let Some(value) = try_new(slot_class, place)? else {
                    bail!(skip);
                };
                let stored = if value == nil() { s4_null()? } else { value };
                values::set_attr(prototype_object, symbol, stored)?;
            }
        } else {
            for known in &supers {
                let super_prototype = slot(known.definition, "prototype")?;
                let super_slots = values::names(slot(known.definition, "slots")?);
                for index in 0..length(super_slots) {
                    let symbol = install_text(super_slots.element(index))?;
                    if !has_attribute(prototype_object, symbol) {
                        values::set_attr(
                            prototype_object,
                            symbol,
                            get_attr(super_prototype, symbol),
                        )?;
                    }
                }
            }
        }
    }
    for &(slot_name, slot_class) in &slots {
        let value = get_attr(prototype_object, install_text(slot_name)?);
        if value == s4_null()? {
            continue;
        }
        let Some(class_definition) =
            cached_class(values::scalar_string_charsxp(slot_class.element(0))?)?
        else {
            bail!(skip);
        };
        if is_true(slot(class_definition, "virtual")?) {
            continue;
        }
        let value_class = dispatch::class_of(value)?;
        let first = values::scalar_string_charsxp(value_class.element(0))?;
        if name_extends(first, cached_class(first)?, slot_class.element(0).bytes())? != Some(true) {
            bail!(skip);
        }
    }
    prototype_object.set_gp_bits(gp::S4_OBJECT, true);

    let Some(representation_class) = cached_class(values::scalar_string("classRepresentation")?)?
    else {
        bail!(skip);
    };
    let Some(extension_class) = cached_class(values::scalar_string("SClassExtension")?)? else {
        bail!(skip);
    };
    let class_name = values::scalar_string_charsxp(name)?;
    values::set_attr(class_name, package_symbol, package)?;

    // The direct extensions, as `setIs()` makes them once the class is complete.
    let slot_names: Vec<Sexp> = slots.iter().map(|&(slot_name, _)| slot_name).collect();
    let mut directs: Vec<Sexp> = Vec::with_capacity(supers.len());
    for known in &supers {
        let extension = direct_extension(
            extension_class,
            class_name,
            package,
            &slot_names,
            known,
            place,
        )?;
        directs.push(extension);
    }
    // The extensions through the first superclass: one in the new class's `contains`, and one
    // in each ancestor's `subclasses`, whose replace method names the superclass with its
    // package.
    let mut through_contains = Vec::with_capacity(ancestors.len());
    let mut through_subclasses = Vec::with_capacity(ancestors.len());
    if let (Some(first), Some(&direct)) = (supers.first(), directs.first()) {
        let extensions = slot(first.definition, "contains")?;
        let first_class_name = slot(first.definition, "className")?;
        for (index, ancestor) in ancestors.iter().enumerate() {
            let to_extension = extensions.element(index);
            let plain = values::scalar_string_charsxp(first.name)?;
            through_contains.push(combine_extensions(
                direct,
                to_extension,
                plain,
                values::scalar_string_charsxp(ancestor.name)?,
                first,
            )?);
            through_subclasses.push(combine_extensions(
                direct,
                to_extension,
                first_class_name,
                values::scalar_string_charsxp(ancestor.name)?,
                first,
            )?);
        }
    }

    let mut contains_entries: Vec<(Sexp, Sexp)> = supers
        .iter()
        .zip(&directs)
        .map(|(known, &extension)| (known.name, extension))
        .collect();
    contains_entries.extend(
        ancestors
            .iter()
            .zip(&through_contains)
            .map(|(known, &extension)| (known.name, extension)),
    );
    sort_by_distance(&mut contains_entries)?;
    let contains_list = if virtual_class {
        named_list(&[])?
    } else if contains_entries.is_empty() {
        values::alloc(SexpType::List, 0)?
    } else {
        named_list(&contains_entries)?
    };
    let slots_list = if slots.is_empty() {
        values::alloc(SexpType::List, 0)?
    } else {
        named_list(&slots)?
    };
    let definition = crate::methods::new_object(representation_class)?;
    crate::heap().root(definition);
    let version = crate::heap().alloc_external_ptr(std::ptr::null_mut(), nil(), nil())?;
    crate::heap().root(version);
    for (slot_name, value) in [
        ("className", class_name),
        ("slots", slots_list),
        ("contains", contains_list),
        ("prototype", prototype_object),
        ("virtual", values::scalar_bool(virtual_class)?),
        ("validity", validity),
        ("access", values::alloc(SexpType::List, 0)?),
        ("package", package),
        ("versionKey", version),
        ("sealed", values::scalar_bool(false)?),
        ("subclasses", values::alloc(SexpType::List, 0)?),
    ] {
        crate::subset::assign_slot(definition, install(slot_name)?, value)?;
    }

    let mut rebuilt = Vec::with_capacity(supers.len());
    for (known, &direct) in supers.iter().zip(&directs) {
        let Some(subclasses) = walked_subclasses(known, name, direct)? else {
            bail!(skip);
        };
        rebuilt.push(subclasses);
    }
    for (known, subclasses) in supers.iter().zip(&rebuilt) {
        record_subclasses(known, subclasses, place, class_table)?;
    }
    for (known, &extension) in ancestors.iter().zip(&through_subclasses) {
        let mut subclasses = list_entries(slot(known.definition, "subclasses")?);
        subclasses.push((name, extension));
        record_subclasses(known, &subclasses, place, class_table)?;
    }
    eval::define(meta, definition, place)?;
    eval::define(install_text(name)?, definition, class_table)?;
    if !supers.is_empty() {
        cache_extends(name, contains_list)?;
    }

    let generator_formals = cons_tagged(missing_arg(), nil(), symbols().dots)?;
    let generator_body = lcons(
        install("new")?,
        cons(class_name, cons(symbols().dots, nil())?)?,
    )?;
    let generator = values::triple(SexpType::Closure, generator_formals, generator_body, place)?;
    for (attribute, value) in [
        ("className", class_name),
        ("package", package),
        ("class", methods_class("classGeneratorFunction")?),
    ] {
        values::set_attr(generator, install(attribute)?, value)?;
    }
    generator.set_gp_bits(gp::S4_OBJECT, true);
    if trace_bails() {
        eprintln!(
            "methods fast path completes setClass(\"{}\")",
            String::from_utf8_lossy(name.bytes())
        );
    }
    Ok(generator)
}

/// A class the new one extends, by name, with its definition.
struct Super {
    name: Sexp,
    definition: Sexp,
}

/// The definition of the class `name` when `setIs()` may rebind it in `place`: a plain class
/// of `package` that is bound there, unsealed, without a data part, a test or a coercion of
/// its own toward its superclasses, and with a prototype for a virtual class to give its
/// subclasses.
unsafe fn local_class(name: Sexp, package: Sexp, place: Sexp) -> R<Option<Sexp>> {
    let Some(definition) = cached_class(values::scalar_string_charsxp(name)?)? else {
        return Ok(None);
    };
    let meta = class_meta_symbol(name.bytes())?;
    if !is_plain_representation(definition)
        || quiet_value(meta, place)? != Some(definition)
        || !assignable(place, meta)
        || slot_text(definition, "package")? != package.element(0).bytes()
        || !is_false(slot(definition, "sealed")?)
        || element(slot(definition, "slots")?, b".Data").is_some()
        || element(slot(definition, "slots")?, b".S3Class").is_some()
    {
        return Ok(None);
    }
    let contains = slot(definition, "contains")?;
    for index in 0..length(contains) {
        if !plain_extension(contains.element(index))? {
            return Ok(None);
        }
    }
    let is_virtual = is_true(slot(definition, "virtual")?);
    if is_virtual && slot(definition, "prototype")? == nil() {
        return Ok(None);
    }
    // A class without slots of its own is virtual unless it extends a class, so a
    // non-virtual one without slots has superclasses that are all virtual.
    if !is_virtual && length(slot(definition, "slots")?) == 0 && length(contains) == 0 {
        return Ok(None);
    }
    Ok(Some(definition))
}

/// Whether an extension is a simple `SClassExtension` that is not to a data part.
unsafe fn plain_extension(extension: Sexp) -> R<bool> {
    let class = values::class_attr(extension);
    Ok(length(class) == 1
        && class.element(0).bytes() == b"SClassExtension"
        && simple(extension)?
        && is_false(slot(extension, "dataPart")?))
}

/// The subclass list of `known` once the class `name` is its direct subclass through
/// `direct`, as `setIs()` builds it with `completeSubclasses()`: it drops the indirect entries,
/// then `.walkClassGraph()` adds each direct subclass's own subclasses that are not direct ones,
/// combined with `.combineExtends()`, and orders the list by distance. Every indirect entry is
/// rebuilt, and a class reached through two direct subclasses appears twice. `None` when an
/// entry is not a plain extension, whose combination the native core does not build.
unsafe fn walked_subclasses(
    known: &Super,
    name: Sexp,
    direct: Sexp,
) -> R<Option<Vec<(Sexp, Sexp)>>> {
    let mut directs = Vec::new();
    for (subclass, extension) in list_entries(slot(known.definition, "subclasses")?) {
        if !plain_extension(extension)? {
            return Ok(None);
        }
        if length(slot(extension, "by")?) == 0 {
            directs.push((subclass, extension));
        }
    }
    directs.push((name, direct));
    let class_name = slot(known.definition, "className")?;
    let mut walked = directs.clone();
    for &(subclass, extension) in &directs {
        if subclass.bytes() == name.bytes() {
            continue;
        }
        let Some(definition) = cached_class(values::scalar_string_charsxp(subclass)?)? else {
            return Ok(None);
        };
        let by = Super {
            name: subclass,
            definition,
        };
        let plain = values::scalar_string_charsxp(subclass)?;
        for (deeper, deeper_extension) in list_entries(slot(definition, "subclasses")?) {
            if directs
                .iter()
                .any(|known| known.0.bytes() == deeper.bytes())
            {
                continue;
            }
            if !plain_extension(deeper_extension)? {
                return Ok(None);
            }
            walked.push((
                deeper,
                combine_extensions(deeper_extension, extension, plain, class_name, &by)?,
            ));
        }
    }
    sort_by_distance(&mut walked)?;
    Ok(Some(walked))
}

/// Whether the dispatch caches the R code fills while defining the class are full already,
/// and no coerce method from the new class to the classes it extends exists for `setIs()` to
/// replace.
unsafe fn definition_caches(name: &[u8], lineage: &[Sexp], prototype: bool) -> R<bool> {
    let Some(generics) = methods_value(".genericTable")? else {
        return Ok(false);
    };
    if generics.kind() != SexpType::Environment {
        return Ok(false);
    }
    let mut initialize_labels = vec![&b"classRepresentation"[..], b"SClassExtension"];
    if prototype {
        initialize_labels.push(b"classPrototypeDef");
    }
    let requirements: [(&str, &[&[u8]]); 3] = [
        ("initialize", &initialize_labels),
        (
            "coerce",
            &[b"NULL#OptionalFunction", b"function#classGeneratorFunction"],
        ),
        ("coerce<-", &[b"classGeneratorFunction#function"]),
    ];
    for (generic_name, labels) in requirements {
        if generic_name != "initialize" && quiet_value(install(generic_name)?, generics)?.is_none()
        {
            return Ok(false);
        }
        let Some((methods_table, all_table)) = generic_tables(generic_name)? else {
            return Ok(false);
        };
        for label in labels {
            if quiet_value(values::install_bytes(label)?, all_table)?.is_none() {
                return Ok(false);
            }
        }
        if generic_name != "initialize" {
            for known in lineage {
                let mut label = name.to_vec();
                label.push(b'#');
                label.extend_from_slice(known.bytes());
                let symbol = values::install_bytes(&label)?;
                if crate::heap().frame_cell(methods_table, symbol).is_some()
                    || crate::heap().frame_cell(all_table, symbol).is_some()
                {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

/// The `.MTable` and `.AllMTable` of a generic of the methods package.
unsafe fn generic_tables(generic_name: &str) -> R<Option<(Sexp, Sexp)>> {
    let Some(generic) = methods_value(generic_name)? else {
        return Ok(None);
    };
    if generic.kind() != SexpType::Closure {
        return Ok(None);
    }
    let (Some(methods_table), Some(all_table)) = (
        quiet_value(install(".MTable")?, generic.tag())?,
        quiet_value(install(".AllMTable")?, generic.tag())?,
    ) else {
        return Ok(None);
    };
    if methods_table.kind() != SexpType::Environment || all_table.kind() != SexpType::Environment {
        return Ok(None);
    }
    Ok(Some((methods_table, all_table)))
}

/// `tryNew(class, where)` for a defined slot class: a virtual class's prototype, or `new()` of
/// the class when no `initialize` method of its own could fail without arguments. `None` when
/// that needs R code that could fail.
unsafe fn try_new(class: Sexp, place: Sexp) -> R<Option<Sexp>> {
    let Some(definition) = cached_class(values::scalar_string_charsxp(class)?)? else {
        return Ok(None);
    };
    if is_true(slot(definition, "virtual")?) {
        return slot(definition, "prototype").map(Some);
    }
    let Some((methods_table, _)) = generic_tables("initialize")? else {
        return Ok(None);
    };
    let contained = values::names(slot(definition, "contains")?);
    let mut labels = vec![class.bytes()];
    labels.extend((0..length(contained)).map(|index| contained.element(index).bytes()));
    for label in labels {
        if crate::heap()
            .frame_cell(methods_table, values::install_bytes(label)?)
            .is_some()
        {
            return Ok(None);
        }
    }
    let Some(new) = methods_value("new")? else {
        return Ok(None);
    };
    eval::eval(values::call(new, &[definition])?, place).map(Some)
}

/// The extension from the new class to a superclass that `makeExtends()` makes once the class
/// is complete: a simple one, whose coerce method rebuilds or relabels the object and whose
/// replace method copies the superclass's slots back. To a virtual class, it coerces by
/// returning the object and replaces only with a value of that class.
unsafe fn direct_extension(
    extension_class: Sexp,
    class_name: Sexp,
    package: Sexp,
    slot_names: &[Sexp],
    known: &Super,
    place: Sexp,
) -> R<Sexp> {
    let super_name = slot(known.definition, "className")?;
    let (Some(simple_coerce), Some(simple_test), Some(simple_replace)) = (
        methods_value(".simpleExtCoerce")?,
        methods_value(".simpleExtTest")?,
        methods_value(".simpleExtReplace")?,
    ) else {
        return Err(crate::unwind::error(
            "the methods package lacks its extension functions",
        ));
    };
    let (coerce, replace) = if is_true(slot(known.definition, "virtual")?) {
        let body = template(
            Template::ReplaceVirtual,
            &[("FROM", class_name), ("TO", super_name)],
        )?;
        let replace = values::triple(SexpType::Closure, simple_replace.car(), body, place)?;
        (simple_coerce, replace)
    } else {
        let super_slots = values::names(slot(known.definition, "slots")?);
        let same_slots = slot_names.len() == length(super_slots)
            && slot_names
                .iter()
                .all(|slot_name| position_of(super_slots, slot_name.bytes()).is_some());
        let coerce_body = if same_slots {
            template(Template::CoerceSame, &[("CLASS", super_name)])?
        } else if length(super_slots) == 0 {
            template(Template::CoerceStrip, &[("CLASS", super_name)])?
        } else {
            template(
                Template::CoerceNew,
                &[
                    ("CLASS", values::scalar_string_charsxp(known.name)?),
                    ("TOSLOTS", super_slots),
                ],
            )?
        };
        // A class without slots has superclasses without slots, which `makeExtends()` takes
        // as the same representation, so replacing relabels the value.
        let replace_body = if slot_names.is_empty() {
            template(Template::ReplaceClass, &[("FROM", class_name)])?
        } else {
            template(Template::ReplaceSlots, &[("TOSLOTS", super_slots)])?
        };
        (
            values::triple(SexpType::Closure, simple_coerce.car(), coerce_body, place)?,
            values::triple(SexpType::Closure, simple_replace.car(), replace_body, place)?,
        )
    };
    let extension = crate::methods::new_object(extension_class)?;
    crate::heap().root(extension);
    for (slot_name, value) in [
        ("subClass", class_name),
        ("superClass", super_name),
        ("package", package),
        ("coerce", coerce),
        ("test", simple_test),
        ("replace", replace),
        ("simple", values::scalar_bool(true)?),
        ("by", alloc_strings(0)?),
        ("dataPart", values::scalar_bool(false)?),
        ("distance", values::scalar_real(1.0)?),
    ] {
        crate::subset::assign_slot(extension, install(slot_name)?, value)?;
    }
    Ok(extension)
}

/// `.combineExtends(by_extension, to_extension, by, to, TRUE)` for two simple extensions that
/// are not to a data part: from the new class through `known` (named `by`) to the class `to`, a
/// character vector kept with its attributes.
unsafe fn combine_extensions(
    by_extension: Sexp,
    to_extension: Sexp,
    by: Sexp,
    to: Sexp,
    known: &Super,
) -> R<Sexp> {
    let from = install("from")?;
    let to_coerce = slot(to_extension, "coerce")?;
    let to_body = crate::bytecode::closure_body(to_coerce);
    let by_body = crate::bytecode::closure_body(slot(by_extension, "coerce")?);
    let coerce_body = if by_body == from && to_body == from {
        from
    } else {
        template(Template::CombineCoerce, &[("E1", by_body), ("E2", to_body)])?
    };
    let coerce = values::triple(
        SexpType::Closure,
        to_coerce.car(),
        coerce_body,
        to_coerce.tag(),
    )?;
    let by_replace = slot(by_extension, "replace")?;
    let by_replace_body = crate::bytecode::closure_body(by_replace);
    let replace_body = if is_true(slot(known.definition, "virtual")?) {
        let skip_replace = slot(to_extension, "replace")?;
        template(
            Template::CombineReplaceVirtual,
            &[
                ("BODY", crate::bytecode::closure_body(skip_replace)),
                ("TO", slot(by_extension, "subClass")?),
            ],
        )?
    } else {
        template(
            Template::CombineReplace,
            &[
                ("BY", by),
                ("TO", to),
                ("STRICT", values::scalar_bool(true)?),
                ("BYEXPR", by_replace_body),
            ],
        )?
    };
    let replace = values::triple(
        SexpType::Closure,
        by_replace.car(),
        replace_body,
        by_replace.tag(),
    )?;
    let distance = values::as_real_scalar(slot(to_extension, "distance")?)
        + values::as_real_scalar(slot(by_extension, "distance")?);
    let combined = values::shallow_duplicate(to_extension)?;
    crate::heap().root(combined);
    for (slot_name, value) in [
        ("coerce", coerce),
        ("replace", replace),
        ("by", slot(to_extension, "subClass")?),
        ("subClass", slot(by_extension, "subClass")?),
        ("distance", values::scalar_real(distance)?),
        ("package", slot(by_extension, "package")?),
    ] {
        crate::subset::assign_slot(combined, install(slot_name)?, value)?;
    }
    Ok(combined)
}

/// Rebinds the definition of a class the new one extends, with `subclasses` as its list of
/// subclasses, where it lives and in the class cache, as `assignClassDef()` does.
unsafe fn record_subclasses(
    known: &Super,
    subclasses: &[(Sexp, Sexp)],
    place: Sexp,
    class_table: Sexp,
) -> R<()> {
    let definition = values::shallow_duplicate(known.definition)?;
    crate::heap().root(definition);
    crate::subset::assign_slot(definition, install("subclasses")?, named_list(subclasses)?)?;
    eval::define(class_meta_symbol(known.name.bytes())?, definition, place)?;
    eval::define(install_text(known.name)?, definition, class_table)?;
    cache_extends(known.name, slot(definition, "contains")?)
}

/// `.cache_class(name, extends(definition))`: the class and the names of what it extends.
unsafe fn cache_extends(name: Sexp, contains: Sexp) -> R<()> {
    let contained = values::names(contains);
    let count = if contained.kind() == SexpType::String {
        length(contained)
    } else {
        0
    };
    let extends = alloc_strings(count + 1)?;
    let heap = crate::heap();
    heap.set_element(extends, 0, name);
    for index in 0..count {
        heap.set_element(extends, index + 1, contained.element(index));
    }
    interp()
        .globals
        .s4_extends
        .insert(name.bytes().into(), extends);
    Ok(())
}

/// The slot names and classes of a `slots` argument or a `representation` list: a character
/// vector or a list of single strings, every element named, the names distinct.
unsafe fn named_classes(value: Sexp) -> Option<Vec<(Sexp, Sexp)>> {
    let names = values::names(value);
    if names.kind() != SexpType::String
        || value.attrib().cdr() != nil()
        || value.has_gp(gp::S4_OBJECT)
        || length(value) == 0
    {
        return None;
    }
    let mut pairs = Vec::with_capacity(length(value));
    for index in 0..length(value) {
        let class = match value.kind() {
            SexpType::String => value.element(index),
            SexpType::List => {
                let item = value.element(index);
                if !is_plain_name(item) {
                    return None;
                }
                item.element(0)
            }
            _ => return None,
        };
        if class == values::na_string() || class.bytes().is_empty() {
            return None;
        }
        pairs.push((names.element(index), class));
    }
    let slot_names: Vec<Sexp> = pairs.iter().map(|&(slot_name, _)| slot_name).collect();
    if !distinct_names(&slot_names)
        || slot_names
            .iter()
            .any(|slot_name| matches!(slot_name.bytes(), b".Data" | b"class" | b".S3Class"))
    {
        return None;
    }
    Some(pairs)
}

/// Whether names are non-empty, not `NA` and distinct.
unsafe fn distinct_names(names: &[Sexp]) -> bool {
    names.iter().enumerate().all(|(index, &name)| {
        name != values::na_string()
            && !name.bytes().is_empty()
            && !names[..index]
                .iter()
                .any(|earlier| earlier.bytes() == name.bytes())
    })
}

/// Whether `symbol` is bound in `env` or an environment it encloses.
unsafe fn bound_from(symbol: Sexp, env: Sexp) -> bool {
    let mut frame = env;
    while frame != values::empty_env() {
        if crate::heap().frame_cell(frame, symbol).is_some() {
            return true;
        }
        frame = frame.enclosure();
    }
    false
}

/// `classMetaName(name)`: the symbol `.__C__name`.
unsafe fn class_meta_symbol(name: &[u8]) -> R<Sexp> {
    let mut text = b".__C__".to_vec();
    text.extend_from_slice(name);
    values::install_bytes(&text)
}

/// The names and elements of a list.
unsafe fn list_entries(list: Sexp) -> Vec<(Sexp, Sexp)> {
    let names = values::names(list);
    if list.kind() != SexpType::List || names.kind() != SexpType::String {
        return Vec::new();
    }
    (0..length(list))
        .map(|index| (names.element(index), list.element(index)))
        .collect()
}

unsafe fn named_list(entries: &[(Sexp, Sexp)]) -> R<Sexp> {
    let heap = crate::heap();
    let list = values::alloc(SexpType::List, entries.len())?;
    let names = alloc_strings(entries.len())?;
    for (index, &(name, value)) in entries.iter().enumerate() {
        heap.set_element(list, index, value);
        heap.set_element(names, index, name);
    }
    values::set_attr(list, symbols().names, names)?;
    Ok(list)
}

/// Orders extensions by distance, keeping the order of equal ones, as `sort.list()` does.
unsafe fn sort_by_distance(entries: &mut [(Sexp, Sexp)]) -> R<()> {
    let mut keyed = Vec::with_capacity(entries.len());
    for &(name, extension) in entries.iter() {
        let distance = values::as_real_scalar(slot(extension, "distance")?);
        keyed.push((distance, name, extension));
    }
    keyed.sort_by(|left, right| left.0.total_cmp(&right.0));
    for (entry, (_, name, extension)) in entries.iter_mut().zip(keyed) {
        *entry = (name, extension);
    }
    Ok(())
}

/// The R expressions the native class definitions fill in, in the order of
/// `METHODS_TEMPLATES`.
#[derive(Clone, Copy)]
enum Template {
    CoerceSame,
    CoerceNew,
    ReplaceSlots,
    ReplaceVirtual,
    CombineCoerce,
    CombineReplaceVirtual,
    CombineReplace,
    CoerceStrip,
    ReplaceClass,
}

/// The expressions of `.simpleCoerceExpr()`, `.simpleReplaceExpr()`, `makeExtends()` and
/// `.combineExtends()` in R 4.6, with the placeholders their `substitute()` calls fill in.
const METHODS_TEMPLATES: &str = r#"list(
    quote({class(from) <- CLASS; from}),
    quote({ value <- methods::new(CLASS)
            for(what in TOSLOTS)
                methods::slot(value, what) <- methods::slot(from, what)
            value }),
    quote({
        for(what in TOSLOTS)
            methods::slot(from, what) <- methods::slot(value, what)
        from
    }),
    quote({
        if(!is(value, TO))
            stop(gettextf("the computation: 'as(object,\"%s\") <- value' is valid when object has class %s only if 'is(value, \"%s\")' is TRUE ('class(value)' was %s)\n",
                 TO, dQuote(FROM), TO, dQuote(class(value))), domain = NA)
        value
    }),
    quote({from <- E1; E2}),
    quote(methods::as(BODY, TO)),
    quote({
        .value <- methods::as(from, BY, STRICT)
        methods::as(.value, TO) <- value
        value <- .value
        BYEXPR
    }),
    quote({ attributes(from) <- NULL; class(from) <- CLASS; from}),
    quote({class(value) <- FROM; value})
)"#;

/// The template `which` with its placeholders replaced, as `substitute()` replaces them: every
/// call is copied and the values go in as they are.
unsafe fn template(which: Template, bindings: &[(&str, Sexp)]) -> R<Sexp> {
    let templates = interp().globals.methods_templates;
    if templates.kind() != SexpType::List || length(templates) <= which as usize {
        return Err(crate::unwind::error("the methods templates are not loaded"));
    }
    let mut symbols = Vec::with_capacity(bindings.len());
    for &(placeholder, value) in bindings {
        symbols.push((install(placeholder)?, value));
    }
    substitute_symbols(templates.element(which as usize), &symbols)
}

unsafe fn substitute_symbols(expression: Sexp, bindings: &[(Sexp, Sexp)]) -> R<Sexp> {
    match expression.kind() {
        SexpType::Symbol => Ok(bindings
            .iter()
            .find(|&&(symbol, _)| symbol == expression)
            .map_or(expression, |&(_, value)| value)),
        SexpType::Language | SexpType::Pairlist => {
            let head = substitute_symbols(expression.car(), bindings)?;
            let rest = substitute_symbols(expression.cdr(), bindings)?;
            values::triple(expression.kind(), head, rest, expression.tag())
        }
        _ => Ok(expression),
    }
}

/// Whether the native definitions are on. Setting `RY_METHODS_NATIVE=0` makes every
/// definition run its R code, for comparing the two.
fn native_definitions() -> bool {
    static NATIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *NATIVE.get_or_init(|| std::env::var_os("RY_METHODS_NATIVE").is_none_or(|value| value != "0"))
}

// Definition helpers.

/// The value of the argument `name` in `frame`, forced as the R body forces it at this point,
/// which runs whatever code the promise holds. `None` when the argument is missing.
unsafe fn forced_arg(frame: Sexp, name: &str) -> R<Option<Sexp>> {
    let Some(cell) = crate::heap().frame_cell(frame, install(name)?) else {
        return Ok(None);
    };
    if cell.has_gp(gp::ACTIVE_BINDING) {
        return Ok(None);
    }
    let value = cell.cdr();
    if value == missing_arg() || value == unbound() {
        return Ok(None);
    }
    eval::value_of(value).map(Some)
}

/// A single, non-empty string without attributes.
unsafe fn is_plain_name(value: Sexp) -> bool {
    value.kind() == SexpType::String
        && length(value) == 1
        && value.attrib() == nil()
        && value.element(0) != values::na_string()
        && !value.element(0).bytes().is_empty()
}

/// The value bound to `name` in the methods namespace. Its bindings are lazy-load promises, and
/// forcing one only reads the saved value, as the R code reading the variable would.
unsafe fn methods_value(name: &str) -> R<Option<Sexp>> {
    let Some(cell) = crate::heap().frame_cell(interp().globals.methods_namespace, install(name)?)
    else {
        return Ok(None);
    };
    if cell.has_gp(gp::ACTIVE_BINDING) {
        return Ok(None);
    }
    eval::value_of(eval::cell_value(cell)?).map(Some)
}

/// `inBasicFuns(name)`: whether the methods package keeps an implicit generic for the base
/// function `name`, which `setGeneric()` treats specially.
unsafe fn in_basic_funs(name: &[u8]) -> R<bool> {
    let Some(functions) = methods_value(".BasicFunsList")? else {
        return Ok(true);
    };
    Ok(match element(functions, name) {
        None => false,
        Some(value) => !is_false(value),
    })
}

#[derive(PartialEq, Eq)]
enum Visible {
    None,
    Function(Sexp),
}

/// `getFunction(name, mustFind = FALSE, where = env)`: the first function bound to `symbol` from
/// `env` outward, up to the base namespace for a namespace and up to the base environment
/// otherwise. Bindings are forced as `get0()` forces them.
unsafe fn visible_function(symbol: Sexp, env: Sexp) -> R<Visible> {
    let last = if crate::envir::is_namespace(env) {
        values::base_namespace()
    } else {
        values::base_env()
    };
    let mut frame = env;
    loop {
        if let Some(cell) = crate::heap().frame_cell(frame, symbol) {
            let value = eval::value_of(eval::cell_value(cell)?)?;
            if values::is_function(value) {
                return Ok(Visible::Function(value));
            }
        }
        if frame == last || frame == values::empty_env() {
            return Ok(Visible::None);
        }
        frame = frame.enclosure();
    }
}

/// `.getFromStandardPackages(name)` finding nothing: `Some(false)` when no standard package
/// namespace defines a function `name`, `None` when the namespaces are not loaded yet, which the
/// R code does first.
unsafe fn standard_packages_define(symbol: Sexp) -> R<Option<bool>> {
    let Some(namespaces) = methods_value(".standardPackageNamespaces")? else {
        return Ok(None);
    };
    if namespaces.kind() != SexpType::Environment {
        return Ok(None);
    }
    let symbols = crate::heap().frame_symbols(namespaces);
    if symbols.is_empty() {
        return Ok(None);
    }
    for namespace_symbol in symbols {
        let namespace = eval::value_of(eval::find_var_in_frame(namespace_symbol, namespaces)?)?;
        if namespace.kind() != SexpType::Environment {
            return Ok(None);
        }
        if let Some(cell) = crate::heap().frame_cell(namespace, symbol) {
            let value = eval::value_of(eval::cell_value(cell)?)?;
            if values::is_function(value) {
                return Ok(Some(true));
            }
        }
    }
    Ok(Some(false))
}

/// Whether `body` is exactly the call `standardGeneric("name")`.
unsafe fn is_standard_generic_call(body: Sexp, name: &[u8]) -> R<bool> {
    let Some([(tag, argument)]) = call_args::<1>(body, "standardGeneric")? else {
        return Ok(false);
    };
    Ok(tag == nil()
        && argument.kind() == SexpType::String
        && length(argument) == 1
        && argument.attrib() == nil()
        && argument.element(0).bytes() == name)
}

/// `getPackageName(env)` where the answer needs no search: the environment's `.packageName`, or
/// `.GlobalEnv` for the global environment.
unsafe fn package_name(env: Sexp) -> R<Option<Sexp>> {
    if env == values::global_env() {
        if crate::heap()
            .frame_cell(env, install(".packageName")?)
            .is_some()
        {
            return Ok(None);
        }
        return values::scalar_string(".GlobalEnv").map(Some);
    }
    match quiet_value(install(".packageName")?, env)? {
        Some(name)
            if name.kind() == SexpType::String
                && length(name) == 1
                && name.attrib() == nil()
                && name.element(0) != values::na_string() =>
        {
            Ok(Some(name))
        }
        _ => Ok(None),
    }
}

/// `.TableMetaName(name, package)`: the symbol `.__T__name:package`.
unsafe fn table_meta_symbol(name: &[u8], package: &[u8]) -> R<Sexp> {
    let mut text = b".__T__".to_vec();
    text.extend_from_slice(name);
    text.push(b':');
    text.extend_from_slice(package);
    values::install_bytes(&text)
}

/// Whether `assign(symbol, value, env)` succeeds without calling an active binding.
unsafe fn assignable(env: Sexp, symbol: Sexp) -> bool {
    match crate::heap().frame_cell(env, symbol) {
        Some(cell) => !cell.has_gp(gp::BINDING_LOCKED | gp::ACTIVE_BINDING),
        None => !env.has_gp(gp::FRAME_LOCKED),
    }
}

/// The names of a formals pairlist, as symbols.
unsafe fn formal_names(formals: Sexp) -> Vec<Sexp> {
    let mut names = Vec::new();
    let mut cell = formals;
    while cell.kind() == SexpType::Pairlist {
        names.push(cell.tag());
        cell = cell.cdr();
    }
    names
}

/// Whether a method's formals have the generic's names and give no argument a default the
/// generic's lacks, so `matchDefaults()` leaves the method as it is.
unsafe fn same_formals(method: Sexp, generic: Sexp) -> bool {
    let (mut method, mut generic) = (method, generic);
    while method.kind() == SexpType::Pairlist && generic.kind() == SexpType::Pairlist {
        if method.tag() != generic.tag()
            || (method.car() == missing_arg() && generic.car() != missing_arg())
        {
            return false;
        }
        method = method.cdr();
        generic = generic.cdr();
    }
    method == nil() && generic == nil()
}

/// `new.env(TRUE, parent)`, whose hash table size decides the order its names list in.
unsafe fn hashed_env(parent: Sexp) -> R<Sexp> {
    let env = values::new_env(parent)?;
    crate::heap().set_frame_hashed(env, 29);
    Ok(env)
}

unsafe fn alloc_strings(count: usize) -> R<Sexp> {
    values::alloc(SexpType::String, count)
}

/// The symbol a `NULL` slot holds.
unsafe fn s4_null() -> R<Sexp> {
    values::install_bytes(b"\x01NULL\x01")
}

/// A class attribute for a class of the methods package.
unsafe fn methods_class(class: &str) -> R<Sexp> {
    let value = values::scalar_string(class)?;
    values::set_attr(
        value,
        install("package")?,
        values::scalar_string("methods")?,
    )?;
    Ok(value)
}

/// Appends the attributes of `from` that `to` lacks, after its own: setting the data part of an
/// S4 function keeps the function's attributes, such as its `srcref`, after the slots.
unsafe fn append_attributes(from: Sexp, to: Sexp) -> R<()> {
    let mut cell = from.attrib();
    while cell.kind() == SexpType::Pairlist {
        if !has_attribute(to, cell.tag()) {
            values::set_attr(to, cell.tag(), cell.car())?;
        }
        cell = cell.cdr();
    }
    Ok(())
}

/// Whether `generic` is a plain `standardGeneric` whose methods dispatch through its own tables:
/// not a group generic, a member of a group, or the generic of a primitive.
unsafe fn is_standard_generic(generic: Sexp) -> R<bool> {
    if generic.kind() != SexpType::Closure || !generic.has_gp(gp::S4_OBJECT) {
        return Ok(false);
    }
    let class = values::class_attr(generic);
    if length(class) != 1 || class.element(0).bytes() != b"standardGeneric" {
        return Ok(false);
    }
    let group = get_attr(generic, install("group")?);
    if group.kind() != SexpType::List || length(group) != 0 {
        return Ok(false);
    }
    let default = get_attr(generic, install("default")?);
    if matches!(default.kind(), SexpType::Builtin | SexpType::Special)
        || values::inherits(default, "internalDispatchMethod")
    {
        return Ok(false);
    }
    for name in ["generic", "package"] {
        let value = get_attr(generic, install(name)?);
        if value.kind() != SexpType::String || length(value) != 1 {
            return Ok(false);
        }
    }
    Ok(get_attr(generic, install("signature")?).kind() == SexpType::String)
}

/// The text of a slot that holds a single string.
unsafe fn slot_text<'a>(object: Sexp, name: &str) -> R<&'a [u8]> {
    Ok(get_attr(object, install(name)?).element(0).bytes())
}

/// `.sigLabel(signature)`: the class names joined by `#`, as a symbol.
unsafe fn signature_label(signature: Sexp) -> R<Sexp> {
    let mut text = Vec::new();
    for index in 0..length(signature) {
        if index > 0 {
            text.push(b'#');
        }
        text.extend_from_slice(signature.element(index).bytes());
    }
    values::install_bytes(&text)
}

/// `.noMlists()`: methods lists are off. R 4.6 removed them, and with them the flag that turned
/// them off, so a methods package without the flag has none.
unsafe fn no_methods_lists() -> R<bool> {
    Ok(methods_value(".noMlistsFlag")?.is_none_or(|value| is_true(value)))
}

/// `cacheOnAssign(env)`: the environment's `.cacheOnAssign` is unset or `TRUE`.
unsafe fn cache_on_assign(env: Sexp) -> R<bool> {
    match crate::heap().frame_cell(env, install(".cacheOnAssign")?) {
        None => Ok(true),
        Some(_) => Ok(quiet_value(install(".cacheOnAssign")?, env)?
            .is_some_and(|value| value == nil() || is_true(value))),
    }
}

/// `.duplicateClassesExist()`: whether two packages define a class of the same name, which makes
/// the methods tables key methods by package too. Unknown counts as yes.
unsafe fn duplicate_classes() -> R<bool> {
    let Some(table) = methods_value(".classTable")? else {
        return Ok(true);
    };
    if table.kind() != SexpType::Environment {
        return Ok(true);
    }
    Ok(!quiet_value(install("#HAS_DUPLICATE_CLASS_NAMES")?, table)?
        .is_some_and(|value| is_false(value)))
}

fn with_label(mut symbols: Vec<Sexp>, label: Sexp) -> Vec<Sexp> {
    if !symbols.contains(&label) {
        symbols.push(label);
    }
    symbols
}
