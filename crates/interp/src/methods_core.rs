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
            self, cons, get_attr, install, lcons, length, missing_arg, nil, scalar_integer,
            symbols, unbound,
        },
    },
    heap::object::{Sexp, SexpType, gp},
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("ryS4New", 1, do_new),
    internal("ryS4Initialize", 1, do_initialize),
    internal("ryS4Valid", 2, do_valid),
    internal("ryS4Is", 2, do_is),
    internal("ryS4Extends", 1, do_extends),
];

/// GNU R 4.3.3's functions that get a fast path, with the [`stable_hash`] of the formals and
/// body each was made from. `initialize` is the default method of the `initialize` generic.
/// Setting `RY_PRINT_METHODS_HASHES` prints the hashes of the loaded ones, for updating this
/// together with `methods_core.R` and the fast paths.
const FAST_PATHS: &[(&str, u64)] = &[
    ("new", 0xed04_11d7_4c52_3b14),
    ("initialize", 0x0cbc_a8bc_242d_4840),
    ("validObject", 0xcfec_1557_eba8_fe90),
    ("is", 0xa975_8041_9461_37c4),
    ("extends", 0x2df8_66a4_f3b4_1f5e),
];

/// Puts the fast paths of `methods_core.R` in front of the functions of the methods namespace
/// `namespace` they were made for, changing each closure in place so that every reference to it
/// gets the fast path.
pub unsafe fn install_fast_paths(namespace: Sexp) -> R<()> {
    let prologues = eval_text(include_str!("methods_core.R"), namespace)?;
    crate::heap().root(prologues);
    let names = values::names(prologues);
    let print = std::env::var_os("RY_PRINT_METHODS_HASHES").is_some();
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
            .map(|&(_, hash)| hash);
        for closure in closures {
            if closure.kind() != SexpType::Closure {
                continue;
            }
            let body = crate::bytecode::closure_body(closure);
            let mut hash = FNV_OFFSET;
            stable_hash(closure.car(), &mut hash);
            stable_hash(body, &mut hash);
            if print {
                eprintln!("{name} {hash:#018x}");
            }
            if Some(hash) != expected {
                continue;
            }
            let block = lcons(
                install("{")?,
                cons(prologues.element(index), cons(body, nil())?)?,
            )?;
            crate::heap().set_cdr(closure, block);
        }
    }
    Ok(())
}

/// Every copy of the `initialize` generic's default method the namespace holds: the generic's
/// `default` slot and the `ANY` entries of its methods tables.
unsafe fn default_initialize_methods(namespace: Sexp) -> R<Vec<Sexp>> {
    let generic = eval::value_of(eval::find_var_in_frame(install("initialize")?, namespace)?)?;
    if generic.kind() != SexpType::Closure {
        return Ok(Vec::new());
    }
    let mut methods = vec![get_attr(generic, install("default")?)];
    let any = install("ANY")?;
    let table = eval::find_var_in_frame(install(".__T__initialize:methods")?, namespace)?;
    let mut tables = vec![eval::value_of(table)?];
    for name in [".MTable", ".AllMTable"] {
        tables.push(eval::value_of(eval::find_var_in_frame(
            install(name)?,
            generic.tag(),
        )?)?);
    }
    for table in tables {
        if table.kind() == SexpType::Environment {
            let method = eval::value_of(eval::find_var_in_frame(any, table)?)?;
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

/// The default `initialize` method, for an object whose arguments all name slots: sets the
/// slots on a copy of `.Object`, binds it as `.Object` and calls `validObject(.Object)` from the
/// method's frame, as the R code does.
unsafe fn do_initialize(call: &Call<'_>) -> R<Sexp> {
    let frame = call.arg(0);
    let done = values::scalar_bool(true)?;
    let skip = values::scalar_bool(false)?;
    let object_symbol = install(".Object")?;
    let Some(object) = quiet_value(object_symbol, frame)? else {
        return Ok(skip);
    };
    let list_dots = lcons(install("list")?, cons(symbols().dots, nil())?)?;
    let args = eval::eval(list_dots, frame)?;
    crate::heap().root(args);
    if length(args) == 0 {
        return Ok(done);
    }
    let class = dispatch::class_of(object)?;
    if length(class) != 1 || basic_classes()?.contains(&class.element(0).bytes()) {
        return Ok(skip);
    }
    let Some(definition) = cached_class(class)? else {
        return Ok(skip);
    };
    let names = values::names(args);
    if names.kind() != SexpType::String {
        return Ok(skip);
    }
    let slots = slot(definition, "slots")?;
    if slots.kind() != SexpType::List {
        return Ok(skip);
    }
    let slot_names = values::names(slots);
    let package = slot(definition, "package")?;
    let mut assignments = Vec::with_capacity(length(args));
    for index in 0..length(args) {
        let name = names.element(index);
        let name_bytes = name.bytes();
        if name_bytes.is_empty()
            || name_bytes == b".Data"
            || (0..index).any(|earlier| names.element(earlier).bytes() == name_bytes)
        {
            return Ok(skip);
        }
        let Some(position) = position_of(slot_names, name_bytes) else {
            return Ok(skip);
        };
        let slot_class = slots.element(position);
        if slot_class.kind() != SexpType::String || length(slot_class) != 1 {
            return Ok(skip);
        }
        let value = args.element(index);
        let value_class = dispatch::class_of(value)?;
        let same = length(value_class) == 1
            && value_class.element(0).bytes() == slot_class.element(0).bytes();
        if !same {
            let Some(slot_definition) = cached_class(slot_class)? else {
                return Ok(skip);
            };
            if slot_class.element(0).bytes() == b"ANY" {
                // `as(value, "ANY")` is the value itself; only the R code's lookup of the
                // value's class is left, which has no effect once the package is loaded.
                if cached_class(value_class)?.is_none() && !package_loaded(package)? {
                    return Ok(skip);
                }
            } else {
                let Some(value_definition) = cached_class(value_class)? else {
                    return Ok(skip);
                };
                let relation = possible_extends(
                    value_class,
                    slot_class.element(0).bytes(),
                    Some(value_definition),
                    Some(slot_definition),
                )?;
                // A value of a class that extends the slot's goes through `as()`, which may
                // cache a coerce method.
                if !matches!(relation, Some(Relation::No)) {
                    return Ok(skip);
                }
            }
        }
        assignments.push((install_text(name)?, value));
    }
    let copy = values::shallow_duplicate(object)?;
    crate::heap().root(copy);
    for (symbol, value) in assignments {
        crate::subset::assign_slot(copy, symbol, value)?;
    }
    eval::define(object_symbol, copy, frame)?;
    let validate = lcons(install("validObject")?, cons(object_symbol, nil())?)?;
    eval::eval(validate, frame)?;
    Ok(done)
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
        if matches!(name.bytes(), b".Data" | b".S3Class") {
            return Ok(false);
        }
        let symbol = install_text(name)?;
        if !has_attribute(object, symbol) {
            return Ok(false);
        }
        let slot_class = slots.element(index);
        if slot_class.kind() != SexpType::String || length(slot_class) != 1 {
            return Ok(false);
        }
        let Some(slot_definition) = cached_class(slot_class)? else {
            return Ok(false);
        };
        let value = crate::subset::get_slot(object, symbol, nil())?;
        let value_class = dispatch::class_of(value)?;
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
