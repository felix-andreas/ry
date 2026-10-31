//! The methods package's native routines: S4 dispatch, slots and class support.
//!
//! The methods package selects methods in R (`.InheritForDispatch` and the tables it caches
//! into) and leaves the per-call fast path to native code: compute the dispatch label from the
//! classes of the generic's signature arguments, look it up in the generic's table of all
//! methods, and run the method in place of the generic. That path, and the small helpers the
//! package's R code reaches through `.Call`, are here.

use crate::{
    builtin_libraries::{NativeCall, NativeFn},
    conditions::{self, Handler},
    dispatch::{self, MethodsState},
    eval::{self, BINDING_MISSING},
    interp,
    unwind::{Jump, R, Unwind, error, error_in},
    values::{self, length, missing_arg, nil, symbol_name, symbols, unbound},
};
use heap::object::{Sexp, SexpType, gp};
use std::rc::Rc;

pub const ROUTINES: &[(&str, NativeFn)] = &[
    ("R_M_setPrimitiveMethods", set_primitive_methods_routine),
    ("R_clear_method_selection", clear_method_selection),
    ("R_dummy_extern_place", dummy_extern_place),
    ("R_el_named", element_named),
    ("R_externalptr_prototype_object", external_pointer_prototype),
    ("R_getClassFromCache", class_from_cache),
    ("R_getGeneric", get_generic_routine),
    ("R_get_slot", get_slot),
    ("R_hasSlot", has_slot_routine),
    ("R_identC", identical_strings),
    ("R_initMethodDispatch", init_method_dispatch),
    ("R_methodsPackageMetaName", package_meta_name),
    ("R_methods_test_MAKE_CLASS", test_make_class),
    ("R_methods_test_NEW", test_new),
    ("R_missingArg", missing_arg_routine),
    ("R_nextMethodCall", next_method_call),
    ("R_quick_method_check", quick_method_check_routine),
    ("R_selectMethod", select_method),
    ("R_set_el_named", set_element_named),
    ("R_set_slot", set_slot),
    ("R_standardGeneric", standard_generic),
    ("do_substitute_direct", substitute_direct),
    ("Rf_allocS4Object", alloc_s4_object),
    ("R_set_method_dispatch", set_method_dispatch),
    ("R_get_primname", primitive_name),
    ("new_object", new_object_routine),
];

/// `R_dispatchGeneric`: selects and runs the method of generic `generic` named `name` for the
/// arguments in the generic's frame `env`.
pub unsafe fn dispatch_generic(name: Sexp, env: Sexp, generic: Sexp) -> R<Sexp> {
    let generic_name = values::first_string(name).unwrap_or_default().into_owned();
    if generic.kind() != SexpType::Closure {
        let class = dispatch::class_of(generic)?;
        return Err(error(format!(
            "expected a generic function or a primitive for dispatch, got an object of class \"{}\"",
            values::first_string(class).unwrap_or_default()
        )));
    }
    let generic_env = generic.tag();
    let table = all_methods_table(generic, generic_env)?;
    let signature = signature_symbols(generic_env)?;
    let mut classes = Vec::with_capacity(signature.len());
    let mut label = String::new();
    for (index, &symbol) in signature.iter().enumerate() {
        let class = if symbol == symbols().dots {
            dots_class(env)?
        } else if bound_missing(symbol, env)? {
            values::scalar_string("missing")?
        } else {
            let value = evaluate_argument(symbol, env, &generic_name)?;
            dispatch_class(value)?
        };
        if index > 0 {
            label.push('#');
        }
        label.push_str(&values::first_string(class).unwrap_or_default());
        classes.push(class);
    }
    let mut method = eval::value_of(eval::find_var_in_frame(values::install(&label)?, table)?)?;
    if method == unbound() {
        method = inherited_method(&classes, generic, table)?;
    }
    if method.is_object() {
        method = load_method(method, name, env)?;
    }
    match method.kind() {
        SexpType::Closure => {
            if values::inherits(method, "internalDispatchMethod") {
                dispatch::deferred_default_marker()
            } else {
                exec_method(method, env)
            }
        }
        // A primitive is never a method; it is the default of a primitive made generic, and
        // the marker tells the primitive to run its own code.
        SexpType::Builtin | SexpType::Special => dispatch::deferred_default_marker(),
        _ => Err(error("invalid object (non-function) used as method")),
    }
}

/// `R_quick_dispatch`: the method already in the table of generic `generic` for the classes of
/// `arguments` (a pairlist of the primitive's matched arguments), or `NULL` when the generic
/// itself must select one.
pub unsafe fn quick_method_check(arguments: Sexp, methods: Sexp, generic: Sexp) -> R<Sexp> {
    let table = if generic.kind() == SexpType::Closure {
        eval::find_var_in_frame(values::install(".AllMTable")?, generic.tag())?
    } else {
        unbound()
    };
    let table = if table.kind() == SexpType::Environment {
        table
    } else if methods.kind() == SexpType::Environment {
        methods
    } else {
        return Ok(nil());
    };
    let signature_length = if generic.kind() == SexpType::Closure {
        let value = eval::find_var_in_frame(values::install(".SigLength")?, generic.tag())?;
        if value == unbound() {
            1
        } else {
            values::as_integer_scalar(value).max(0) as usize
        }
    } else {
        1
    };
    let mut label = String::new();
    let mut argument = arguments;
    for index in 0..signature_length {
        // Arguments past the ones supplied are missing.
        let value = if argument.kind() == SexpType::Pairlist {
            argument.car()
        } else {
            missing_arg()
        };
        let class = if value == missing_arg() {
            "missing".to_owned()
        } else {
            let value = eval::value_of(value)?;
            let class = dispatch_class(value)?;
            values::first_string(class).unwrap_or_default().into_owned()
        };
        if index > 0 {
            label.push('#');
        }
        label.push_str(&class);
        if argument.kind() == SexpType::Pairlist {
            argument = argument.cdr();
        }
    }
    let method = eval::find_var_in_frame(values::install(&label)?, table)?;
    if method == unbound() {
        return Ok(nil());
    }
    eval::value_of(method)
}

/// `R_execMethod`: runs `method` in place of the generic whose frame is `generic_env`. The
/// method's frame binds each formal to the generic's binding of it, so a missing argument stays
/// missing and gets its default from the method's own formals, evaluated in the method's frame.
/// The frame also gets the dispatch variables, and takes over the generic's call, caller and
/// arguments, which is what `sys.call()`, `parent.frame()` and `match.call()` see.
pub unsafe fn exec_method(method: Sexp, generic_env: Sexp) -> R<Sexp> {
    let (call, sys_parent, promargs) = interp()
        .frames
        .iter()
        .rev()
        .find(|frame| frame.env == generic_env)
        .map(|frame| (frame.call, frame.sys_parent, frame.promargs))
        .ok_or_else(|| error("could not find the generic function's frame"))?;
    let heap = crate::heap();
    let method_env = values::new_env(method.tag())?;
    let mut formal = method.car();
    while formal.kind() == SexpType::Pairlist {
        let symbol = formal.tag();
        let cell = heap.frame_cell(generic_env, symbol).ok_or_else(|| {
            error(format!(
                "could not find symbol \"{}\" in environment of the generic function",
                symbol_name(symbol)
            ))
        })?;
        let missing = cell.has_gp(BINDING_MISSING) || cell.cdr() == missing_arg();
        let mut value = cell.cdr();
        if missing && formal.car() != missing_arg() {
            value = eval::make_promise(formal.car(), method_env)?;
        }
        eval::define(symbol, value, method_env)?;
        if missing && let Some(new_cell) = heap.frame_cell(method_env, symbol) {
            new_cell.set_gp_bits(BINDING_MISSING, true);
        }
        formal = formal.cdr();
    }
    for name in [".defined", ".Method", ".target", ".Methods"] {
        let symbol = values::install(name)?;
        let value = eval::find_var_in_frame(symbol, generic_env)?;
        if value != unbound() {
            eval::define(symbol, value, method_env)?;
        }
    }
    let generic_symbol = values::install(".Generic")?;
    let generic = eval::find_var(generic_symbol, generic_env)?;
    if generic == unbound() {
        return Err(error(
            "generic function name not found in the environment of the generic",
        ));
    }
    eval::define(generic_symbol, generic, method_env)?;
    eval::run_closure(call, method, method_env, sys_parent, promargs)
}

/// `R_do_new_object`: a new object of the class `class_def` defines, a copy of its prototype,
/// marked as S4 when the prototype is a plain S4 object or the class comes from a package.
pub unsafe fn new_object(class_def: Sexp) -> R<Sexp> {
    if class_def == nil() {
        return Err(error(
            "C level NEW macro called with null class definition pointer",
        ));
    }
    let virtual_flag = slot_value(class_def, "virtual")?;
    let class_name = slot_value(class_def, "className")?;
    if values::as_logical_scalar(virtual_flag) != 0 {
        return Err(error(format!(
            "trying to generate an object from a virtual class (\"{}\")",
            values::first_string(class_name).unwrap_or_default()
        )));
    }
    let prototype = slot_value(class_def, "prototype")?;
    let value = values::duplicate(prototype)?;
    let package = values::get_attr(class_name, values::install("package")?);
    if value.kind() == SexpType::Object || package != nil() {
        values::set_attr(value, symbols().class, class_name)?;
        value.set_gp_bits(gp::S4_OBJECT, true);
    }
    Ok(value)
}

/// `R_do_MAKE_CLASS`: the definition of class `name`, from the methods package's `getClass`.
pub unsafe fn make_class(name: &str) -> R<Sexp> {
    let get_class = methods_function("getClass")?;
    let call = values::call(get_class, &[values::scalar_string(name)?])?;
    eval::eval(call, values::global_env())
}

/// `R_has_slot`: whether `object` has slot `name`. Every object but a plain S4 one has a data
/// part.
pub unsafe fn has_slot(object: Sexp, name: Sexp) -> R<bool> {
    let symbol = slot_symbol(name)?;
    if symbol == symbols().dot_data && object.kind() != SexpType::Object {
        return Ok(true);
    }
    Ok(values::get_attr(object, symbol) != nil())
}

/// The symbol a slot name stands for: a symbol, or a single string.
pub unsafe fn slot_symbol(name: Sexp) -> R<Sexp> {
    match name.kind() {
        SexpType::Symbol => Ok(name),
        SexpType::String if length(name) == 1 => values::install_bytes(name.element(0).bytes()),
        _ => Err(error("invalid type or length for slot name")),
    }
}

/// `R_data_class(object, TRUE)`: the one class S4 dispatch uses for `object`. A class attribute
/// of length one is kept as it is, with its `package` attribute.
pub unsafe fn dispatch_class(object: Sexp) -> R<Sexp> {
    first_class(dispatch::class_of(object)?)
}

// Selection.

/// The generic's `.AllMTable`, which the methods package sets up on first use.
unsafe fn all_methods_table(generic: Sexp, generic_env: Sexp) -> R<Sexp> {
    let symbol = values::install(".AllMTable")?;
    let table = eval::value_of(eval::find_var_in_frame(symbol, generic_env)?)?;
    if table.kind() == SexpType::Environment {
        return Ok(table);
    }
    let get_table = methods_function(".getMethodsTable")?;
    let call = values::call(
        get_table,
        &[
            generic,
            generic_env,
            values::scalar_bool(true)?,
            values::scalar_bool(true)?,
        ],
    )?;
    let table = eval::eval(call, values::base_env())?;
    if table.kind() != SexpType::Environment {
        return Err(error("invalid methods table request"));
    }
    Ok(table)
}

/// The generic's signature arguments that take part in dispatch: the first `.SigLength` of
/// `.SigArgs`.
unsafe fn signature_symbols(generic_env: Sexp) -> R<Vec<Sexp>> {
    let arguments = eval::value_of(eval::find_var_in_frame(
        values::install(".SigArgs")?,
        generic_env,
    )?)?;
    let count = eval::value_of(eval::find_var_in_frame(
        values::install(".SigLength")?,
        generic_env,
    )?)?;
    if arguments == unbound() || count == unbound() {
        return Err(error(
            "the generic function's environment has no signature information",
        ));
    }
    let count = (values::as_integer_scalar(count).max(0) as usize).min(length(arguments));
    let mut result = Vec::with_capacity(count);
    for index in 0..count {
        let symbol = match arguments.kind() {
            SexpType::String => values::install_bytes(arguments.element(index).bytes())?,
            SexpType::List => {
                let element = arguments.element(index);
                if element.kind() == SexpType::Symbol {
                    element
                } else {
                    slot_symbol(element)?
                }
            }
            _ => {
                return Err(error(
                    "invalid signature arguments in the generic function's environment",
                ));
            }
        };
        result.push(symbol);
    }
    Ok(result)
}

/// Whether the generic's own binding of `symbol` is missing. Only the binding counts: an
/// argument passed on from a caller where it is missing is not missing here, so dispatch
/// evaluates it and reports the error.
unsafe fn bound_missing(symbol: Sexp, env: Sexp) -> R<bool> {
    let cell = crate::heap().frame_cell(env, symbol).ok_or_else(|| {
        error(format!(
            "could not find symbol '{}' in frame of call",
            symbol_name(symbol)
        ))
    })?;
    Ok(cell.has_gp(BINDING_MISSING) || cell.cdr() == missing_arg())
}

/// The class of `...` for dispatch, from the methods package's `.dotsClass`.
unsafe fn dots_class(env: Sexp) -> R<Sexp> {
    let function = methods_function(".dotsClass")?;
    let call = values::call(function, &[symbols().dots])?;
    first_class(eval::eval(call, env)?)
}

unsafe fn first_class(class: Sexp) -> R<Sexp> {
    if length(class) > 1 {
        values::scalar_string_charsxp(values::string_elt(class, 0))
    } else {
        Ok(class)
    }
}

/// Forces a signature argument. GNU R evaluates it under an error handler that reports which
/// argument failed, and that handler's call, `h(simpleError(msg, call))`, is the call the error
/// is reported against.
unsafe fn evaluate_argument(symbol: Sexp, env: Sexp, generic_name: &str) -> R<Sexp> {
    match try_eval(symbol, env)? {
        Ok(value) => Ok(value),
        Err(message) => {
            let simple_error = values::call(
                values::install("simpleError")?,
                &[values::install("msg")?, values::install("call")?],
            )?;
            let handler_call = values::call(values::install("h")?, &[simple_error])?;
            Err(error_in(
                handler_call,
                format!(
                    "error in evaluating the argument '{}' in selecting a method for function '{generic_name}': {message}",
                    symbol_name(symbol)
                ),
            ))
        }
    }
}

/// Evaluates `expression` in `env` under an exiting handler for errors: `Err` holds the
/// message of the error it caught.
unsafe fn try_eval(expression: Sexp, env: Sexp) -> R<Result<Sexp, String>> {
    let state = interp();
    let saved = state.handlers.clone();
    let target = values::new_env(values::empty_env())?;
    state.handlers = Some(Rc::new(Handler {
        class: values::mkchar("error")?,
        handler: nil(),
        calling: false,
        parent_env: env,
        target,
        next: saved.clone(),
    }));
    let result = eval::eval(expression, env);
    interp().handlers = saved;
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(Unwind::Jump(jump)) => match *jump {
            Jump::Return {
                env: returned_to,
                value,
            } if returned_to == target => {
                let condition = if value.kind() == SexpType::List && length(value) > 0 {
                    value.element(0)
                } else {
                    nil()
                };
                if condition == nil() {
                    Ok(Err(interp().error_message.clone()))
                } else {
                    Ok(Err(conditions::condition_message(condition)))
                }
            }
            other => Err(Unwind::Jump(Box::new(other))),
        },
        Err(other) => Err(other),
    }
}

/// `.InheritForDispatch(classes, fdef, mtable)`: selects an inherited method and caches it in
/// the table. The call names the function by value, as GNU R's does, which shows in its errors.
unsafe fn inherited_method(classes: &[Sexp], generic: Sexp, table: Sexp) -> R<Sexp> {
    let function = methods_function(".InheritForDispatch")?;
    let classes = values::list_vector(classes)?;
    let call = values::call(function, &[classes, generic, table])?;
    eval::eval(call, values::base_env())
}

/// `R_loadMethod`: binds the dispatch variables of method definition `method` in the generic's
/// frame. A plain `MethodDefinition` is handled here; any other class of method goes through
/// the methods package's `loadMethod` generic, except when loading a method of `loadMethod`
/// itself, which would recurse.
unsafe fn load_method(method: Sexp, name: Sexp, env: Sexp) -> R<Sexp> {
    let mut extra = false;
    let mut attribute = method.attrib();
    while attribute.kind() == SexpType::Pairlist {
        let tag = attribute.tag();
        match &*symbol_name(tag) {
            "target" => eval::define(values::install(".target")?, attribute.car(), env)?,
            "defined" => eval::define(values::install(".defined")?, attribute.car(), env)?,
            "generic" | "class" | "srcref" => {}
            _ => extra = true,
        }
        attribute = attribute.cdr();
    }
    eval::define(values::install(".Method")?, method, env)?;
    if !extra || values::first_string(name).as_deref() == Some("loadMethod") {
        return Ok(method);
    }
    let function = methods_function("loadMethod")?;
    let call = values::call(function, &[method, name, env])?;
    eval::eval(call, methods_namespace()?)
}

// The routines.

unsafe fn set_primitive_methods_routine(call: &NativeCall<'_>) -> R<Sexp> {
    let (name, op, code, generic, methods) = (
        call.arg(0),
        call.arg(1),
        call.arg(2),
        call.arg(3),
        call.arg(4),
    );
    let code_text = values::first_string(code)
        .ok_or_else(|| call.error("argument 'code' must be a character string"))?
        .into_owned();
    let primitive_name = values::first_string(name).unwrap_or_default().into_owned();
    if op == nil() && primitive_name.is_empty() {
        let globals = &mut interp().globals;
        let previous = globals.allow_primitive_methods;
        match code_text.as_bytes().first() {
            Some(b'C' | b'c') => globals.allow_primitive_methods = false,
            Some(b'S' | b's') => globals.allow_primitive_methods = true,
            _ => {}
        }
        return values::scalar_bool(previous);
    }
    let op = if matches!(op.kind(), SexpType::Builtin | SexpType::Special) {
        op
    } else {
        crate::builtins::primitive(&primitive_name).ok_or_else(|| {
            call.error(format!(
                "object '{primitive_name}' is not a primitive function"
            ))
        })?
    };
    dispatch::set_primitive_methods(op, &code_text, generic, methods)?;
    Ok(name)
}

unsafe fn clear_method_selection(_call: &NativeCall<'_>) -> R<Sexp> {
    Ok(nil())
}

unsafe fn dummy_extern_place(call: &NativeCall<'_>) -> R<Sexp> {
    Err(call.error("calling the C routine used as an initializer for 'externalptr' objects"))
}

unsafe fn element_named(call: &NativeCall<'_>) -> R<Sexp> {
    let name = element_name(call, call.arg(1))?;
    Ok(values::list_get(call.arg(0), &name))
}

unsafe fn set_element_named(call: &NativeCall<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let name = element_name(call, call.arg(1))?;
    let value = call.arg(2);
    if object.kind() != SexpType::List {
        return Err(call.error("R_set_el_named: the object must be a list"));
    }
    let names = values::names(object);
    let count = length(object);
    let position = (0..count).find(|&index| {
        names.kind() == SexpType::String && names.element(index).bytes() == name.as_bytes()
    });
    let result = match position {
        Some(index) => {
            let result = values::shallow_duplicate(object)?;
            values::set_elt(result, index, value);
            result
        }
        None => {
            let mut items = values::list_items(object);
            items.push(value);
            let result = values::list_vector(&items)?;
            values::copy_most_attributes(object, result, false)?;
            let mut new_names: Vec<String> = (0..count)
                .map(|index| {
                    if names.kind() == SexpType::String {
                        values::char_str(names.element(index)).into_owned()
                    } else {
                        String::new()
                    }
                })
                .collect();
            new_names.push(name);
            values::set_attr(result, symbols().names, values::string_vector(&new_names)?)?;
            result
        }
    };
    Ok(result)
}

unsafe fn element_name(call: &NativeCall<'_>, what: Sexp) -> R<String> {
    if what.kind() == SexpType::String && length(what) == 1 {
        return Ok(values::char_str(what.element(0)).into_owned());
    }
    if what.kind() == SexpType::Symbol {
        return Ok(symbol_name(what).into_owned());
    }
    Err(call.error("invalid element name: expected a single string"))
}

unsafe fn external_pointer_prototype(_call: &NativeCall<'_>) -> R<Sexp> {
    let heap = crate::heap();
    let pointer = heap.alloc_external_ptr(std::ptr::null_mut(), nil(), nil())?;
    heap.root(pointer);
    Ok(pointer)
}

/// `R_getClassFromCache(class, table)`: the cached definition of the class `class` names, or
/// `NULL`. A `package` attribute on the name must match the definition's package; a class
/// definition passed in is returned as it is.
unsafe fn class_from_cache(call: &NativeCall<'_>) -> R<Sexp> {
    let (class, table) = (call.arg(0), call.arg(1));
    match class.kind() {
        SexpType::String => {
            if length(class) == 0 {
                return Ok(nil());
            }
            let symbol = values::install_bytes(class.element(0).bytes())?;
            let value = eval::value_of(eval::find_var_in_frame(symbol, table)?)?;
            if value == unbound() {
                return Ok(nil());
            }
            let package_symbol = values::install("package")?;
            let package = values::get_attr(class, package_symbol);
            if package.kind() == SexpType::String && length(package) > 0 {
                let defined_package = values::get_attr(value, package_symbol);
                if defined_package.kind() == SexpType::String
                    && length(defined_package) == 1
                    && defined_package.element(0).bytes() != package.element(0).bytes()
                {
                    return Ok(nil());
                }
            }
            Ok(value)
        }
        _ if class.has_gp(gp::S4_OBJECT) => Ok(class),
        _ => {
            Err(call.error("class should be either a character-string name or a class definition"))
        }
    }
}

unsafe fn get_generic_routine(call: &NativeCall<'_>) -> R<Sexp> {
    let (name, must_find, env, package) = (call.arg(0), call.arg(1), call.arg(2), call.arg(3));
    let symbol = match name.kind() {
        SexpType::Symbol => name,
        SexpType::String if length(name) >= 1 => values::install_bytes(name.element(0).bytes())?,
        _ => {
            return Err(
                call.error("invalid generic function name: expected a character string or a name")
            );
        }
    };
    if env.kind() != SexpType::Environment {
        return Err(call.error("invalid environment for the generic function"));
    }
    let package = values::first_string(package)
        .unwrap_or_default()
        .into_owned();
    let generic = find_generic(symbol, env, &package)?;
    if generic == nil() && values::as_logical_scalar(must_find) == 1 {
        return Err(call.error(format!(
            "no generic function definition found for '{}'",
            symbol_name(symbol)
        )));
    }
    Ok(generic)
}

/// The first generic function bound to `symbol` from `env` outward (from `package`, unless
/// that is empty): a closure with a `generic` attribute, or the generic the methods package
/// set up for a primitive.
unsafe fn find_generic(symbol: Sexp, env: Sexp, package: &str) -> R<Sexp> {
    let heap = crate::heap();
    let generic_symbol = values::install("generic")?;
    let package_symbol = values::install("package")?;
    let mut frame = env;
    while frame != values::empty_env() {
        if let Some(cell) = heap.frame_cell(frame, symbol) {
            let value = eval::value_of(eval::cell_value(cell)?)?;
            let candidate = match value.kind() {
                SexpType::Closure if values::get_attr(value, generic_symbol) != nil() => value,
                SexpType::Builtin | SexpType::Special => {
                    dispatch::primitive_methods(value).map_or(nil(), |(generic, _)| generic)
                }
                _ => nil(),
            };
            if candidate != nil() {
                let defined = values::get_attr(candidate, package_symbol);
                let matches = package.is_empty()
                    || values::first_string(defined).is_none_or(|defined| defined == package);
                if matches {
                    return Ok(candidate);
                }
            }
        }
        frame = frame.enclosure();
    }
    Ok(nil())
}

unsafe fn get_slot(call: &NativeCall<'_>) -> R<Sexp> {
    let symbol =
        slot_symbol(call.arg(1)).map_err(|_| call.error("invalid type or length for slot name"))?;
    crate::subset::get_slot(call.arg(0), symbol, call.call)
}

unsafe fn set_slot(call: &NativeCall<'_>) -> R<Sexp> {
    let name = call.arg(1);
    if name.kind() != SexpType::String || length(name) != 1 {
        return Err(call.error("invalid type or length for slot name"));
    }
    let symbol = values::install_bytes(name.element(0).bytes())?;
    crate::subset::assign_slot(call.arg(0), symbol, call.arg(2))
}

unsafe fn has_slot_routine(call: &NativeCall<'_>) -> R<Sexp> {
    values::scalar_bool(has_slot(call.arg(0), call.arg(1))?)
}

unsafe fn identical_strings(call: &NativeCall<'_>) -> R<Sexp> {
    let (left, right) = (call.arg(0), call.arg(1));
    let same = left.kind() == SexpType::String
        && right.kind() == SexpType::String
        && length(left) == 1
        && length(right) == 1
        && left.element(0).bytes() == right.element(0).bytes();
    values::scalar_bool(same)
}

/// Turns S4 dispatch on, with `where` as the methods namespace.
unsafe fn init_method_dispatch(call: &NativeCall<'_>) -> R<Sexp> {
    let namespace = call.arg(0);
    let globals = &mut interp().globals;
    if namespace.kind() == SexpType::Environment {
        globals.methods_namespace = namespace;
    }
    globals.methods_dispatch = true;
    Ok(namespace)
}

/// `.__<prefix>__<name>`, with `:<package>` appended for a non-empty package: the name the
/// methods package stores its metadata objects under.
unsafe fn package_meta_name(call: &NativeCall<'_>) -> R<Sexp> {
    let mut parts = Vec::with_capacity(3);
    for (index, what) in ["prefix", "name", "package"].iter().enumerate() {
        let value = call.arg(index);
        if value.kind() != SexpType::String || length(value) != 1 {
            return Err(call.error(format!(
                "The internal argument \"{what}\" to methodsPackageMetaName must be a single string"
            )));
        }
        parts.push(values::char_str(value.element(0)).into_owned());
    }
    let mut result = format!(".__{}__{}", parts[0], parts[1]);
    if !parts[2].is_empty() {
        result.push(':');
        result.push_str(&parts[2]);
    }
    values::scalar_string(&result)
}

unsafe fn test_make_class(call: &NativeCall<'_>) -> R<Sexp> {
    let name = values::first_string(call.arg(0))
        .ok_or_else(|| call.error("C level MAKE_CLASS macro called with NULL string pointer"))?
        .into_owned();
    make_class(&name)
}

unsafe fn test_new(call: &NativeCall<'_>) -> R<Sexp> {
    let name = values::first_string(call.arg(0))
        .ok_or_else(|| call.error("C level MAKE_CLASS macro called with NULL string pointer"))?
        .into_owned();
    new_object(make_class(&name)?)
}

unsafe fn missing_arg_routine(call: &NativeCall<'_>) -> R<Sexp> {
    let (symbol, env) = (call.arg(0), call.arg(1));
    let symbol = match symbol.kind() {
        SexpType::Symbol => symbol,
        SexpType::String if length(symbol) >= 1 => {
            values::install_bytes(symbol.element(0).bytes())?
        }
        _ => {
            let class = dispatch::class_of(symbol)?;
            return Err(call.error(format!(
                "invalid symbol in checking for missing argument in method dispatch: expected a name, got an object of class \"{}\"",
                values::first_string(class).unwrap_or_default()
            )));
        }
    };
    let env = if env == nil() {
        values::global_env()
    } else {
        env
    };
    if env.kind() != SexpType::Environment {
        let class = dispatch::class_of(env)?;
        return Err(call.error(format!(
            "invalid environment in checking for missing argument, '{}', in methods dispatch: supplied an object of class \"{}\"",
            symbol_name(symbol),
            values::first_string(class).unwrap_or_default()
        )));
    }
    if crate::heap().frame_cell(env, symbol).is_none() {
        return Err(call.error(format!(
            "could not find symbol '{}' in frame of call",
            symbol_name(symbol)
        )));
    }
    values::scalar_bool(eval::is_missing(symbol, env)?)
}

/// `R_nextMethodCall(matched_call, env)`: evaluates the method call `matched_call` (from
/// `match.call(expand.dots = FALSE)`) with `.nextMethod` as the function and every supplied
/// argument passed on as the method frame's own binding of it (`... = ...` for the dots, which
/// the call shows in errors). A primitive next method is
/// called directly, with its S4 dispatch suppressed so that it runs its own code.
unsafe fn next_method_call(call: &NativeCall<'_>) -> R<Sexp> {
    let (matched_call, env) = (call.arg(0), call.arg(1));
    let next_symbol = values::install(".nextMethod")?;
    let mut op = eval::value_of(eval::find_var_in_frame(next_symbol, env)?)?;
    if op == unbound() {
        return Err(call.error(
            "internal error in 'callNextMethod': '.nextMethod' was not assigned in the frame of the method call",
        ));
    }
    let mut primitive = matches!(op.kind(), SexpType::Builtin | SexpType::Special);
    if !primitive && values::inherits(op, "internalDispatchMethod") {
        let generic = eval::value_of(eval::find_var_in_frame(values::install(".Generic")?, env)?)?;
        let name = values::first_string(generic).ok_or_else(|| {
            call.error("internal error in 'callNextMethod': '.Generic' was not assigned in the frame of the method call")
        })?;
        op = crate::builtins::primitive(&name)
            .ok_or_else(|| call.error(format!("'{name}' is not a primitive function")))?;
        primitive = true;
    }
    let mut items = vec![(nil(), if primitive { op } else { next_symbol })];
    for (tag, value) in values::pairlist_items(matched_call.cdr()) {
        if tag != nil() && value != missing_arg() {
            items.push((tag, tag));
        } else {
            items.push((tag, value));
        }
    }
    let next_call = values::pairlist(&items, SexpType::Language)?;
    if !primitive {
        return eval::eval(next_call, env);
    }
    let index = op.primitive_index();
    let previous = interp()
        .globals
        .primitive_methods
        .get(&index)
        .map(|entry| entry.state);
    if let Some(entry) = interp().globals.primitive_methods.get_mut(&index) {
        entry.state = MethodsState::Suppressed;
    }
    let result = eval::eval(next_call, env);
    if let (Some(state), Some(entry)) =
        (previous, interp().globals.primitive_methods.get_mut(&index))
    {
        entry.state = state;
    }
    result
}

unsafe fn quick_method_check_routine(call: &NativeCall<'_>) -> R<Sexp> {
    quick_method_check(call.arg(0), call.arg(1), call.arg(2))
}

/// `R_selectMethod(f, env, mlist, evalArgs)`: method selection in a `MethodsList`, the
/// representation of methods the package used before methods tables. Each level of the list
/// dispatches on one argument (its `argument` slot) through the named list in its `methods`
/// slot, falling back to `"ANY"`.
unsafe fn select_method(call: &NativeCall<'_>) -> R<Sexp> {
    let (env, mut methods_list, evaluate) = (call.arg(1), call.arg(2), call.arg(3));
    let evaluate = values::as_logical_scalar(evaluate) == 1;
    loop {
        if values::is_function(methods_list) || methods_list == nil() {
            return Ok(methods_list);
        }
        let argument = slot_value(methods_list, "argument")?;
        let methods = slot_value(methods_list, "methods")?;
        let class = if argument.kind() != SexpType::Symbol
            || bound_missing(argument, env).unwrap_or(true)
        {
            "missing".to_owned()
        } else {
            let value = if evaluate {
                eval::eval(argument, env)?
            } else {
                eval::value_of(eval::find_var_in_frame(argument, env)?)?
            };
            let class = dispatch_class(value)?;
            values::first_string(class).unwrap_or_default().into_owned()
        };
        let mut selected = values::list_get(methods, &class);
        if selected == nil() {
            selected = values::list_get(methods, "ANY");
        }
        methods_list = selected;
    }
}

unsafe fn standard_generic(call: &NativeCall<'_>) -> R<Sexp> {
    dispatch_generic(call.arg(0), call.arg(1), call.arg(2))
}

/// `substitute(object, frame)` where `object` is the value itself, not the expression that
/// computes it. A list `frame` is turned into an environment first.
unsafe fn substitute_direct(call: &NativeCall<'_>) -> R<Sexp> {
    let (object, frame) = (call.arg(0), call.arg(1));
    let env = match frame.kind() {
        SexpType::Environment => frame,
        SexpType::List | SexpType::Pairlist => {
            let env = values::new_env(values::base_env())?;
            let items = if frame.kind() == SexpType::List {
                let names = values::names(frame);
                (0..length(frame))
                    .map(|index| {
                        let tag = if names.kind() == SexpType::String
                            && !names.element(index).bytes().is_empty()
                        {
                            values::install_bytes(names.element(index).bytes())
                        } else {
                            Ok(nil())
                        };
                        tag.map(|tag| (tag, frame.element(index)))
                    })
                    .collect::<R<Vec<_>>>()?
            } else {
                values::pairlist_items(frame)
            };
            for (tag, value) in items {
                if tag != nil() {
                    eval::define(tag, value, env)?;
                }
            }
            env
        }
        _ => return Err(call.error("invalid list for substitution")),
    };
    eval::substitute(object, env)
}

unsafe fn alloc_s4_object(_call: &NativeCall<'_>) -> R<Sexp> {
    let object = values::triple(SexpType::Object, nil(), nil(), nil())?;
    object.set_gp_bits(gp::S4_OBJECT, true);
    Ok(object)
}

/// Reports whether table dispatch is on, which it always is here: GNU R can switch back to its
/// older `MethodsList` dispatch, but the tables are the only representation ry dispatches on.
unsafe fn set_method_dispatch(call: &NativeCall<'_>) -> R<Sexp> {
    let previous = interp().globals.methods_dispatch;
    if values::as_logical_scalar(call.arg(0)) == 1 {
        interp().globals.methods_dispatch = true;
    }
    values::scalar_bool(previous)
}

unsafe fn primitive_name(call: &NativeCall<'_>) -> R<Sexp> {
    let object = call.arg(0);
    if !matches!(object.kind(), SexpType::Builtin | SexpType::Special) {
        return Err(call.error("'R_get_primname' called on a non-primitive"));
    }
    values::scalar_string(values::primitive_name(object))
}

unsafe fn new_object_routine(call: &NativeCall<'_>) -> R<Sexp> {
    new_object(call.arg(0))
}

// Helpers.

unsafe fn slot_value(object: Sexp, name: &str) -> R<Sexp> {
    crate::subset::get_slot(object, values::install(name)?, nil())
}

unsafe fn methods_namespace() -> R<Sexp> {
    let namespace = interp().globals.methods_namespace;
    if namespace.kind() == SexpType::Environment {
        Ok(namespace)
    } else {
        Err(error("the methods package is not loaded"))
    }
}

/// A function of the methods namespace, looked up by name.
unsafe fn methods_function(name: &str) -> R<Sexp> {
    let namespace = methods_namespace()?;
    let value = eval::value_of(eval::find_var_in_frame(values::install(name)?, namespace)?)?;
    if !values::is_function(value) {
        return Err(error(format!(
            "could not find function \"{name}\" in the methods package"
        )));
    }
    Ok(value)
}
