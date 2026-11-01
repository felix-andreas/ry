//! S3 dispatch: `UseMethod`, `NextMethod`, the internal generics (`length`, `[`, ...), and the
//! group generics `Ops`, `Math`, `Summary` and `Complex`. Also the class queries the dispatch
//! reads (`class`, `oldClass`, `inherits`), and the hook `standardGeneric` calls once the methods
//! package installs S4 dispatch.

use crate::{
    Frame,
    builtins::{Arg, BuiltinDef, Call, Generic, Kind, builtin, internal, special},
    eval::{self, Args, apply_closure, call_builtin, forced_promise, value_of},
    interp,
    unwind::{R, error},
    values::{
        self, char_str, get_attr, length, missing_arg, nil, scalar_string, string_vector,
        symbol_name, symbols, unbound,
    },
};
use heap::object::{Sexp, SexpType, gp};

pub const BUILTINS: &[BuiltinDef] = &[
    special("UseMethod", -1, do_usemethod).keep_visibility(),
    internal("NextMethod", -1, do_nextmethod).keep_visibility(),
    builtin("class", 1, do_class),
    builtin("oldClass", 1, do_old_class),
    builtin("oldClass<-", 2, do_set_old_class),
    builtin("class<-", 2, do_set_class),
    builtin("unclass", 1, do_unclass),
    internal("inherits", 3, do_inherits),
    builtin(".class2", 1, |call| unsafe { implicit_class(call.arg(0)) }),
    builtin(".cache_class", 2, |call| unsafe { do_cache_class(call) }),
    builtin(".isMethodsDispatchOn", -1, do_is_methods_dispatch_on),
    builtin("isS4", 1, |call| unsafe {
        values::scalar_bool(call.arg(0).has_gp(gp::S4_OBJECT))
    }),
    internal("setS4Object", 3, do_set_s4_object),
    internal("standardGeneric", -1, do_standard_generic),
];

/// The class vector `class()` reports.
pub unsafe fn class_of(object: Sexp) -> R<Sexp> {
    let class = values::class_attr(object);
    if class != nil() && length(class) > 0 {
        return Ok(class);
    }
    let dim = get_attr(object, symbols().dim);
    if dim != nil() {
        return if length(dim) == 2 {
            string_vector(&["matrix", "array"])
        } else {
            scalar_string("array")
        };
    }
    scalar_string(basic_class(object, false))
}

/// The class `UseMethod` dispatches on: for an S4 object, its class and every class it
/// extends; otherwise the class attribute, or the implicit class, which adds `matrix`/`array`,
/// and for numbers both the type and `numeric`.
pub unsafe fn implicit_class(object: Sexp) -> R<Sexp> {
    let class = values::class_attr(object);
    if class != nil() && length(class) > 0 {
        if object.has_gp(gp::S4_OBJECT) {
            return s4_extends(class);
        }
        return Ok(class);
    }
    let mut classes: Vec<&str> = Vec::new();
    let dim = get_attr(object, symbols().dim);
    if dim != nil() {
        if length(dim) == 2 {
            classes.push("matrix");
        }
        classes.push("array");
    }
    match object.kind() {
        SexpType::Integer => classes.extend(["integer", "numeric"]),
        SexpType::Real => classes.extend(["double", "numeric"]),
        _ => classes.push(basic_class(object, true)),
    }
    string_vector(&classes)
}

/// An S4 class's S3 class vector, from the methods package's `.extendsForS3`, cached by class
/// name as GNU R does. Without the methods package the class attribute stands alone.
unsafe fn s4_extends(class: Sexp) -> R<Sexp> {
    let name = class.element(0).bytes();
    if let Some(&extends) = interp().globals.s4_extends.get(name) {
        return Ok(extends);
    }
    let namespace = interp().globals.methods_namespace;
    if namespace == nil() {
        return Ok(class);
    }
    let function = eval::value_of(eval::find_var_in_frame(
        values::install(".extendsForS3")?,
        namespace,
    )?)?;
    if !values::is_function(function) {
        return Ok(class);
    }
    let call = values::call(function, &[class])?;
    let extends = eval::eval(call, values::base_env())?;
    interp().globals.s4_extends.insert(name.into(), extends);
    Ok(extends)
}

/// `.cache_class(class, extends)`: records the S3 class vector of an S4 class, or with
/// `extends = NULL` forgets it.
unsafe fn do_cache_class(call: &Call<'_>) -> R<Sexp> {
    let class = call.arg(0);
    let extends = call.arg(1);
    if class.kind() != SexpType::String || length(class) < 1 {
        return Err(call.error("invalid class argument to internal .class_cache"));
    }
    let name: Box<[u8]> = class.element(0).bytes().into();
    if extends == nil() {
        interp().globals.s4_extends.remove(&name);
    } else {
        interp().globals.s4_extends.insert(name, extends);
    }
    Ok(extends)
}

unsafe fn basic_class(object: Sexp, for_dispatch: bool) -> &'static str {
    match object.kind() {
        SexpType::Closure | SexpType::Special | SexpType::Builtin => "function",
        SexpType::Integer => "integer",
        SexpType::Real => {
            if for_dispatch {
                "double"
            } else {
                "numeric"
            }
        }
        SexpType::Symbol => "name",
        SexpType::Language => {
            let head = object.car();
            if head.kind() == SexpType::Symbol {
                match head.car().bytes() {
                    b"if" => "if",
                    b"while" => "while",
                    b"for" => "for",
                    b"=" => "=",
                    b"<-" => "<-",
                    b"(" => "(",
                    b"{" => "{",
                    _ => "call",
                }
            } else {
                "call"
            }
        }
        SexpType::Record => crate::records::native_kind(object).class_name(),
        kind => kind.name(),
    }
}

// Method lookup.

/// `topenv`: the nearest enclosing global environment, base environment, namespace or package
/// environment.
pub unsafe fn top_env(mut env: Sexp) -> Sexp {
    let empty = values::empty_env();
    let package_name = symbols().dot_package_name;
    while env != empty {
        if env == values::global_env()
            || env == values::base_env()
            || env == values::base_namespace()
            || crate::envir::is_namespace(env)
            || crate::envir::is_package_env(env)
            || crate::heap().frame_cell(env, package_name).is_some()
        {
            return env;
        }
        env = env.enclosure();
    }
    values::global_env()
}

/// Looks up the S3 method `name`: from the calling environment to its top environment, then in
/// the generic's namespace's registered-methods table, then along the search path.
pub unsafe fn lookup_method(name: Sexp, call_env: Sexp, def_env: Sexp) -> R<Option<Sexp>> {
    let top = top_env(call_env);
    let mut env = call_env;
    loop {
        if let Some(function) = function_in_frame(name, env)? {
            return Ok(Some(function));
        }
        if env == top || env == values::empty_env() {
            break;
        }
        env = env.enclosure();
    }
    let def_top = top_env(def_env);
    let table = eval::find_var_in_frame(symbols().dot_s3_methods_table, def_top)?;
    let table = value_of(table)?;
    if table.kind() == SexpType::Environment {
        let value = eval::find_var_in_frame(name, table)?;
        if value != unbound() {
            let value = value_of(value)?;
            if values::is_function(value) {
                return Ok(Some(value));
            }
        }
    }
    let mut env = if top == values::global_env() {
        values::base_env()
    } else {
        top.enclosure()
    };
    while env != values::empty_env() {
        if let Some(function) = function_in_frame(name, env)? {
            return Ok(Some(function));
        }
        if env == values::global_env() {
            env = values::base_env();
            continue;
        }
        env = env.enclosure();
    }
    Ok(None)
}

unsafe fn function_in_frame(name: Sexp, env: Sexp) -> R<Option<Sexp>> {
    let value = eval::find_var_in_frame(name, env)?;
    if value == unbound() {
        return Ok(None);
    }
    let value = value_of(value)?;
    Ok(values::is_function(value).then_some(value))
}

unsafe fn method_symbol(generic: &str, class: &str) -> R<Sexp> {
    values::install(&format!("{generic}.{class}"))
}

/// The dispatch variables a method frame gets.
unsafe fn dispatch_vars(
    generic: &str,
    classes: Sexp,
    method: Sexp,
    call_env: Sexp,
    def_env: Sexp,
    group: Option<&str>,
) -> R<Vec<(Sexp, Sexp)>> {
    let symbols = symbols();
    let mut vars = vec![
        (symbols.dot_generic, scalar_string(generic)?),
        (symbols.dot_class, classes),
        (symbols.dot_method, method),
        (symbols.dot_generic_call_env, call_env),
        (symbols.dot_generic_def_env, def_env),
    ];
    if let Some(group) = group {
        vars.push((symbols.dot_group, scalar_string(group)?));
    }
    Ok(vars)
}

/// The classes from `index` on, as `.Class` holds them.
unsafe fn class_suffix(classes: Sexp, index: usize) -> R<Sexp> {
    let count = length(classes);
    let suffix = values::alloc(SexpType::String, count.saturating_sub(index))?;
    for (target, source) in (index..count).enumerate() {
        values::set_elt(suffix, target, classes.element(source));
    }
    Ok(suffix)
}

/// What `usemethod` found: the method and the dispatch variables for its frame.
struct Found {
    method: Sexp,
    method_name: Sexp,
    vars: Vec<(Sexp, Sexp)>,
}

unsafe fn find_s3_method(
    generic: &str,
    object: Sexp,
    call_env: Sexp,
    def_env: Sexp,
    group: Option<&str>,
) -> R<Option<Found>> {
    let classes = implicit_class(object)?;
    let classes_count = length(classes);
    for index in 0..classes_count {
        let class = char_str(classes.element(index));
        let name = method_symbol(generic, &class)?;
        let mut found = lookup_method(name, call_env, def_env)?;
        let mut method_name = name;
        if found.is_none()
            && let Some(group) = group
        {
            method_name = method_symbol(group, &class)?;
            found = lookup_method(method_name, call_env, def_env)?;
        }
        if let Some(method) = found {
            let suffix = class_suffix(classes, index)?;
            let method_string = scalar_string(&symbol_name(method_name))?;
            let vars = dispatch_vars(generic, suffix, method_string, call_env, def_env, group)?;
            return Ok(Some(Found {
                method,
                method_name,
                vars,
            }));
        }
    }
    let default = method_symbol(generic, "default")?;
    if let Some(method) = lookup_method(default, call_env, def_env)? {
        let method_string = scalar_string(&symbol_name(default))?;
        let empty = values::alloc(SexpType::String, 0)?;
        let vars = dispatch_vars(generic, empty, method_string, call_env, def_env, group)?;
        return Ok(Some(Found {
            method,
            method_name: default,
            vars,
        }));
    }
    Ok(None)
}

/// A copy of `call` that calls `name` instead.
unsafe fn renamed_call(call: Sexp, name: Sexp) -> R<Sexp> {
    if call.kind() != SexpType::Language {
        return values::call(name, &[]);
    }
    let copy = values::lcons(name, call.cdr())?;
    Ok(copy)
}

// UseMethod.

unsafe fn do_usemethod(call: &Call<'_>) -> R<Sexp> {
    let env = call.env;
    let state = interp();
    let Some(position) = state.frames.iter().rposition(|frame| frame.env == env) else {
        return Err(call.error("UseMethod called from outside a function"));
    };
    if call.is_empty() {
        return Err(call.error("'UseMethod' called with no arguments"));
    }
    let generic = eval::eval(call.arg(0), env)?;
    if generic.kind() != SexpType::String || length(generic) != 1 {
        return Err(call.error("'generic' argument must be a character string"));
    }
    let generic_name = char_str(generic.element(0)).into_owned();
    let (frame_call, function, promargs, sys_parent) = {
        let frame: &Frame = &interp().frames[position];
        (frame.call, frame.function, frame.promargs, frame.sys_parent)
    };
    let object = if call.len() >= 2 {
        eval::eval(call.arg(1), env)?
    } else {
        match dispatch_object(function, env)? {
            // GNU R's `GetObject` falls back to the first supplied argument when the first
            // formal was not supplied, so `f(y = obj)` dispatches on `obj`.
            object if object == nil() && first_formal_missing(function, env)? => {
                first_supplied(promargs)?
            }
            object => object,
        }
    };
    let def_env = match lookup_generic_definition(&generic_name, env, sys_parent)? {
        Some(definition) if definition.kind() == SexpType::Closure => definition.tag(),
        _ => values::base_namespace(),
    };
    let Some(found) = find_s3_method(&generic_name, object, sys_parent, def_env, None)? else {
        let class = implicit_class(object)?;
        let classes: Vec<String> = (0..length(class))
            .map(|index| char_str(class.element(index)).into_owned())
            .collect();
        let shown = if classes.len() == 1 {
            format!("\"{}\"", classes[0])
        } else {
            format!("\"c('{}')\"", classes.join("', '"))
        };
        return Err(error(format!(
            "no applicable method for '{generic_name}' applied to an object of class {shown}"
        )));
    };
    let mut vars = found.vars;
    // Local variables the generic defined before dispatching are visible to the method.
    if function.kind() == SexpType::Closure {
        let formals: Vec<Sexp> = values::pairlist_items(function.car())
            .into_iter()
            .map(|(tag, _)| tag)
            .collect();
        for symbol in crate::heap().frame_symbols(env) {
            if !formals.contains(&symbol) && !vars.iter().any(|(var, _)| *var == symbol) {
                if let Some(cell) = crate::heap().frame_cell(env, symbol) {
                    vars.push((symbol, cell.cdr()));
                }
            }
        }
    }
    let args = eval::unevaluated_args(promargs);
    let new_call = renamed_call(frame_call, found.method_name)?;
    let result = apply_method(new_call, found.method, &args, sys_parent, &vars)?;
    Err(crate::unwind::jump(crate::Jump::Return {
        env,
        value: result,
    }))
}

/// The generic function named `name` as seen from the generic's frame.
unsafe fn lookup_generic_definition(name: &str, env: Sexp, caller: Sexp) -> R<Option<Sexp>> {
    let symbol = values::install(name)?;
    for start in [env.enclosure(), caller] {
        let mut frame = start;
        while frame != values::empty_env() {
            if let Some(function) = function_in_frame(symbol, frame)? {
                return Ok(Some(function));
            }
            frame = frame.enclosure();
        }
    }
    Ok(None)
}

/// The object a generic dispatches on: its first argument's value.
unsafe fn dispatch_object(function: Sexp, env: Sexp) -> R<Sexp> {
    if function.kind() != SexpType::Closure {
        return Ok(nil());
    }
    let formals = function.car();
    if formals.kind() != SexpType::Pairlist {
        return Ok(nil());
    }
    let first = formals.tag();
    if first == symbols().dots {
        let dots = eval::find_var_in_frame(first, env)?;
        if dots.kind() == SexpType::Dots {
            return value_of(dots.car());
        }
        return Ok(nil());
    }
    // GNU R dispatches on the first supplied argument, so a generic called without one, even
    // where the formal has a default, dispatches on NULL.
    if crate::heap()
        .frame_cell(env, first)
        .is_some_and(|cell| cell.has_gp(gp::BINDING_MISSING))
    {
        return Ok(nil());
    }
    let value = eval::find_var_in_frame(first, env)?;
    if value == missing_arg() {
        return Ok(nil());
    }
    if value == unbound() {
        return Ok(nil());
    }
    value_of(value)
}

unsafe fn first_formal_missing(function: Sexp, env: Sexp) -> R<bool> {
    let formals = function.car();
    if formals.kind() != SexpType::Pairlist || formals.tag() == symbols().dots {
        return Ok(false);
    }
    let first = formals.tag();
    if crate::heap()
        .frame_cell(env, first)
        .is_some_and(|cell| cell.has_gp(gp::BINDING_MISSING))
    {
        return Ok(true);
    }
    let value = eval::find_var_in_frame(first, env)?;
    Ok(value == missing_arg() || value == unbound())
}

/// The value of the first supplied argument among a call's matched arguments.
unsafe fn first_supplied(mut promargs: Sexp) -> R<Sexp> {
    while promargs.kind() == SexpType::Pairlist {
        let value = promargs.car();
        if value.kind() == SexpType::Dots {
            return value_of(value.car());
        }
        if value != missing_arg() {
            return value_of(value);
        }
        promargs = promargs.cdr();
    }
    Ok(nil())
}

unsafe fn apply_method(
    call: Sexp,
    method: Sexp,
    args: &[Arg],
    sys_parent: Sexp,
    vars: &[(Sexp, Sexp)],
) -> R<Sexp> {
    match method.kind() {
        SexpType::Closure => apply_closure(call, method, args, sys_parent, Some(vars)),
        SexpType::Builtin => {
            let definition = eval::builtin_def(method)?;
            let mut forced = Vec::with_capacity(args.len());
            for arg in args {
                let value = if arg.value == missing_arg() {
                    arg.value
                } else {
                    value_of(arg.value)?
                };
                forced.push(Arg {
                    tag: arg.tag,
                    value,
                });
            }
            call_builtin(definition, call, method, &forced, sys_parent)
        }
        _ => eval::apply(call, method, args, sys_parent),
    }
}

// NextMethod.

unsafe fn do_nextmethod(call: &Call<'_>) -> R<Sexp> {
    let env = call.env;
    let state = interp();
    // The NextMethod closure's frame is `env`; the method frame is the one it was called from.
    let Some(own) = state.frames.iter().rposition(|frame| frame.env == env) else {
        return Err(call.error("NextMethod called from outside a method dispatch"));
    };
    let method_env = state.frames[own].sys_parent;
    let Some(position) = state
        .frames
        .iter()
        .rposition(|frame| frame.env == method_env)
    else {
        return Err(call.error("NextMethod called from outside a method dispatch"));
    };
    let (frame_call, function, promargs, sys_parent) = {
        let frame = &interp().frames[position];
        (frame.call, frame.function, frame.promargs, frame.sys_parent)
    };
    let symbols = symbols();
    let read = |symbol: Sexp| -> R<Sexp> {
        let value = eval::find_var_in_frame(symbol, method_env)?;
        Ok(if value == unbound() {
            value
        } else {
            value_of(value)?
        })
    };
    // The generic: the argument, else `.Generic`, else the called function's name.
    let mut generic = call.arg(0);
    if generic == nil() || generic == missing_arg() {
        generic = read(symbols.dot_generic)?;
    }
    if generic == unbound() || generic == nil() {
        let head = frame_call.car();
        if head.kind() == SexpType::Symbol {
            generic = scalar_string(&symbol_name(head))?;
        } else {
            return Err(call.error("generic function not specified"));
        }
    }
    if generic.kind() != SexpType::String || length(generic) != 1 {
        return Err(call.error("invalid generic argument to 'NextMethod'"));
    }
    let generic_name = char_str(generic.element(0)).into_owned();
    let mut classes = read(symbols.dot_class)?;
    if classes == unbound() {
        let object = dispatch_object(function, method_env)?;
        classes = implicit_class(object)?;
    }
    let group = read(symbols.dot_group)?;
    let group_name = if group != unbound() && group.kind() == SexpType::String && length(group) == 1
    {
        Some(char_str(group.element(0)).into_owned())
    } else {
        None
    };
    let call_env = {
        let value = read(symbols.dot_generic_call_env)?;
        if value == unbound() {
            sys_parent
        } else {
            value
        }
    };
    let def_env = {
        let value = read(symbols.dot_generic_def_env)?;
        if value == unbound() {
            values::base_namespace()
        } else {
            value
        }
    };
    // The current method's name decides where in `.Class` dispatch resumes.
    let current = read(symbols.dot_method)?;
    let current_names: Vec<String> = if current != unbound() && current.kind() == SexpType::String {
        (0..length(current))
            .filter_map(|index| {
                let text = char_str(current.element(index)).into_owned();
                (!text.is_empty()).then_some(text)
            })
            .collect()
    } else if frame_call.car().kind() == SexpType::Symbol {
        vec![symbol_name(frame_call.car()).into_owned()]
    } else {
        Vec::new()
    };
    let class_names: Vec<String> = (0..length(classes))
        .map(|index| char_str(classes.element(index)).into_owned())
        .collect();
    let mut start = 0;
    for (index, class) in class_names.iter().enumerate() {
        let candidates = [
            format!("{generic_name}.{class}"),
            group_name
                .as_ref()
                .map_or(String::new(), |group| format!("{group}.{class}")),
        ];
        if current_names.iter().any(|name| candidates.contains(name)) {
            start = index + 1;
            break;
        }
    }
    // Find the next method along the remaining classes.
    let mut found: Option<(Sexp, Sexp, usize)> = None;
    for (index, class) in class_names.iter().enumerate().skip(start) {
        let name = method_symbol(&generic_name, class)?;
        if let Some(method) = lookup_method(name, call_env, def_env)? {
            found = Some((method, name, index));
            break;
        }
        if let Some(group) = &group_name {
            let name = method_symbol(group, class)?;
            if let Some(method) = lookup_method(name, call_env, def_env)? {
                found = Some((method, name, index));
                break;
            }
        }
    }
    let (method, method_name, next_index) = match found {
        Some(found) => found,
        None => {
            let default = method_symbol(&generic_name, "default")?;
            if let Some(method) = lookup_method(default, call_env, def_env)? {
                (method, default, class_names.len())
            } else {
                let generic_symbol = values::install(&generic_name)?;
                let function = eval::find_fun(generic_symbol, values::base_env())
                    .ok()
                    .filter(|function| function.kind() != SexpType::Closure)
                    .or_else(|| crate::builtins::primitive(&generic_name));
                match function {
                    Some(function) => (function, generic_symbol, class_names.len()),
                    None => {
                        return Err(call.error(format!("no more methods for '{generic_name}'")));
                    }
                }
            }
        }
    };
    // The arguments: the method's own, with each formal replaced by a promise of its current
    // value in the method's frame, so changes the method made are seen.
    let mut args = eval::unevaluated_args(promargs);
    if function.kind() == SexpType::Closure {
        let matched = eval::match_args(function.car(), &args, frame_call)?;
        let formals: Vec<Sexp> = values::pairlist_items(function.car())
            .into_iter()
            .map(|(tag, _)| tag)
            .collect();
        for (formal, value) in formals.iter().zip(matched.iter()) {
            if *formal == symbols.dots || *value == missing_arg() {
                continue;
            }
            if let Some(arg) = args.iter_mut().find(|arg| arg.value == *value) {
                arg.value = eval::make_promise(*formal, method_env)?;
            }
        }
    }
    // Named arguments passed to NextMethod replace or extend the method's arguments.
    for extra in call.args.iter().skip(2) {
        if extra.tag != nil() {
            if let Some(existing) = args.iter_mut().find(|arg| arg.tag == extra.tag) {
                existing.value = extra.value;
                continue;
            }
        }
        args.push(*extra);
    }
    let next_classes = class_suffix(classes, (next_index + 1).min(class_names.len()))?;
    values::set_attr(next_classes, symbols.previous, classes)?;
    // For a group generic `.Method` has one entry per operand, empty for an operand that did
    // not dispatch; the next method keeps that shape, as `Ops.factor` relies on.
    let method_string = if group_name.is_some()
        && current != unbound()
        && current.kind() == SexpType::String
        && length(current) > 1
    {
        let name = symbol_name(method_name).into_owned();
        let entries: Vec<String> = (0..length(current))
            .map(|index| {
                if char_str(current.element(index)).is_empty() {
                    String::new()
                } else {
                    name.clone()
                }
            })
            .collect();
        string_vector(&entries)?
    } else {
        scalar_string(&symbol_name(method_name))?
    };
    let vars = dispatch_vars(
        &generic_name,
        next_classes,
        method_string,
        call_env,
        def_env,
        group_name.as_deref(),
    )?;
    let new_call = renamed_call(frame_call, method_name)?;
    match method.kind() {
        SexpType::Closure => apply_closure(new_call, method, &args, method_env, Some(&vars)),
        _ => {
            let definition = eval::builtin_def(method)?;
            // A special evaluates its arguments itself, so it gets the promises, which
            // evaluate to their values without running a value that is a call. A
            // dispatch-or-eval special's function takes values, so it gets them forced.
            let special = matches!(definition.kind, Kind::Special | Kind::InternalSpecial)
                && !definition.is_dispatch_or_eval();
            let mut forced = Vec::with_capacity(args.len());
            for arg in &args {
                let value = if arg.value == missing_arg() || special {
                    arg.value
                } else {
                    value_of(arg.value)?
                };
                forced.push(Arg {
                    tag: arg.tag,
                    value,
                });
            }
            // The call names the default method, as in GNU R's "Error in `[.default`(x, 5)",
            // which also tells a special that dispatches internally not to dispatch again.
            let default_name = format!("{generic_name}.default");
            let primitive_call = renamed_call(frame_call, values::install(&default_name)?)?;
            call_builtin(definition, primitive_call, method, &forced, method_env)
        }
    }
}

// Dispatch from builtins.

/// Dispatches a builtin call on its arguments' classes, if any is an object with a method.
/// Returns `None` when the builtin itself should run.
pub unsafe fn try_builtin_dispatch(
    definition: &BuiltinDef,
    call: Sexp,
    op: Sexp,
    args: &[Arg],
    env: Sexp,
) -> R<Option<Sexp>> {
    let Some(first) = args.first() else {
        return Ok(None);
    };
    match definition.generic {
        Generic::None => Ok(None),
        Generic::Internal => {
            if !first.value.is_object() {
                return Ok(None);
            }
            if first.value.has_gp(gp::S4_OBJECT)
                && let Some(value) = s4_dispatch(op, call, args, env)?
            {
                return Ok(Some(value));
            }
            // `seq.int` is the primitive behind the `seq` generic, so it finds `seq` methods.
            let generic = if definition.name == "seq.int" {
                "seq"
            } else {
                definition.name
            };
            internal_dispatch(generic, call, args, env)
        }
        Generic::Ops | Generic::MatrixOps => {
            let any_object = args.iter().take(2).any(|arg| arg.value.is_object());
            if !any_object {
                return Ok(None);
            }
            if args
                .iter()
                .take(2)
                .any(|arg| arg.value.has_gp(gp::S4_OBJECT))
                && let Some(value) = s4_dispatch(op, call, args, env)?
            {
                return Ok(Some(value));
            }
            let group = if definition.generic == Generic::Ops {
                "Ops"
            } else {
                "matrixOps"
            };
            group_dispatch(group, definition.name, call, args, env)
        }
        Generic::Summary => {
            // GNU R's `fixup_NaRm` moves `na.rm` last, adding `na.rm = FALSE` when the call has
            // none, before it looks for an object to dispatch on, so a method may declare
            // `na.rm` without a default. The method's call holds the evaluated arguments, as
            // `do_summary` builds it.
            let na_rm = symbols().na_rm;
            let mut fixed: Vec<Arg> = args
                .iter()
                .filter(|arg| arg.tag != na_rm)
                .copied()
                .collect();
            let Some(first) = fixed.first().copied() else {
                return Ok(None);
            };
            if !first.value.is_object() {
                return Ok(None);
            }
            if first.value.has_gp(gp::S4_OBJECT)
                && let Some(value) = s4_dispatch(op, call, args, env)?
            {
                return Ok(Some(value));
            }
            let value = match args.iter().rev().find(|arg| arg.tag == na_rm) {
                Some(arg) => arg.value,
                None => values::scalar_logical(0)?,
            };
            fixed.push(Arg { tag: na_rm, value });
            let head = if call.kind() == SexpType::Language {
                call.car()
            } else {
                op
            };
            let mut items = vec![(nil(), head)];
            items.extend(fixed.iter().map(|arg| (arg.tag, arg.value)));
            let fixed_call = values::pairlist(&items, SexpType::Language)?;
            group_dispatch("Summary", definition.name, fixed_call, &fixed, env)
        }
        Generic::Math | Generic::Complex => {
            if !first.value.is_object() {
                return Ok(None);
            }
            if first.value.has_gp(gp::S4_OBJECT)
                && let Some(value) = s4_dispatch(op, call, args, env)?
            {
                return Ok(Some(value));
            }
            let group = if definition.generic == Generic::Math {
                "Math"
            } else {
                "Complex"
            };
            group_dispatch(group, definition.name, call, args, env)
        }
    }
}

/// The arguments as promises that hold their values, with the call's expressions as code, as
/// GNU R passes them to a method.
unsafe fn promised_args(call: Sexp, args: &[Arg]) -> R<Vec<Arg>> {
    let expressions = if call.kind() == SexpType::Language {
        values::pairlist_items(call.cdr())
    } else {
        Vec::new()
    };
    let mut result = Vec::with_capacity(args.len());
    for (index, arg) in args.iter().enumerate() {
        let code = expressions
            .get(index)
            .map(|(_, expression)| *expression)
            .filter(|expression| *expression != symbols().dots)
            .unwrap_or(arg.value);
        let value = if arg.value == missing_arg() || arg.value.kind() == SexpType::Promise {
            arg.value
        } else {
            forced_promise(code, arg.value)?
        };
        result.push(Arg {
            tag: arg.tag,
            value,
        });
    }
    Ok(result)
}

/// S3 dispatch for a builtin that evaluated its own arguments (`$`, `$<-`). A call named
/// `<generic>.default` comes from `NextMethod` and runs the builtin's own code.
pub unsafe fn dispatch_builtin(name: &str, call: Sexp, args: &[Arg], env: Sexp) -> R<Option<Sexp>> {
    if call.kind() == SexpType::Language
        && call.car().kind() == SexpType::Symbol
        && symbol_name(call.car()).ends_with(".default")
    {
        return Ok(None);
    }
    if args
        .first()
        .is_some_and(|arg| arg.value.has_gp(gp::S4_OBJECT))
        && let Some(op) = crate::builtins::primitive(name)
        && let Some(value) = s4_dispatch(op, call, args, env)?
    {
        return Ok(Some(value));
    }
    internal_dispatch(name, call, args, env)
}

/// GNU R's `DispatchOrEval`, for a [`BuiltinDef::dispatch_or_eval`] special called with the
/// unevaluated `arguments`. Only the first argument is evaluated before dispatch, so a method
/// receives the others as unevaluated promises. Returns the method's value, or the evaluated
/// arguments when the primitive's own code should run.
pub unsafe fn dispatch_or_eval(
    definition: &BuiltinDef,
    call: Sexp,
    op: Sexp,
    arguments: Sexp,
    env: Sexp,
    args: &mut Args,
) -> R<Option<Sexp>> {
    let first = arguments.car();
    if arguments.kind() != SexpType::Pairlist || first == missing_arg() {
        eval::evaluate_args_into(arguments, env, true, call, args)?;
        return Ok(None);
    }
    // `x[...]` takes its object from the dots, so every argument becomes a promise up front, and
    // the first keeps its promise so a method's `substitute(x)` sees the caller's expression.
    if first == symbols().dots {
        let promised = eval::promise_args(arguments, env)?;
        let Some(head) = promised.first() else {
            return Ok(None);
        };
        if head.value == missing_arg() {
            *args = promised;
            return Ok(None);
        }
        let object = value_of(head.value)?;
        crate::heap().root(object);
        return dispatch_promised(definition, call, op, &promised, env, args);
    }
    let object = eval::eval(first, env)?;
    crate::heap().root(object);
    dispatch_evaluated_subject(definition, call, op, arguments, object, env, args)
}

/// [`dispatch_or_eval`] after the first argument of `arguments` evaluated to `object`. A frame
/// subject runs its query here, because the query reads the other arguments as expressions.
pub unsafe fn dispatch_evaluated_subject(
    definition: &BuiltinDef,
    call: Sexp,
    op: Sexp,
    arguments: Sexp,
    object: Sexp,
    env: Sexp,
    args: &mut Args,
) -> R<Option<Sexp>> {
    if !object.is_object() {
        args.push(Arg {
            tag: arguments.tag(),
            value: object,
        });
        eval::evaluate_args_into(arguments.cdr(), env, true, call, args)?;
        return Ok(None);
    }
    if definition.name == "[" && crate::frame::is_frame(object) {
        return crate::frame::run(call, object, env).map(Some);
    }
    if definition.name == "["
        && values::inherits(object, "data.table")
        && let Some(value) = crate::data_table::query(call, object, env)?
    {
        return Ok(Some(value));
    }
    let mut promised: Args = smallvec::smallvec![Arg {
        tag: arguments.tag(),
        value: forced_promise(arguments.car(), object)?,
    }];
    promised.extend(eval::promise_args(arguments.cdr(), env)?);
    dispatch_promised(definition, call, op, &promised, env, args)
}

unsafe fn dispatch_promised(
    definition: &BuiltinDef,
    call: Sexp,
    op: Sexp,
    promised: &Args,
    env: Sexp,
    args: &mut Args,
) -> R<Option<Sexp>> {
    let object = value_of(promised[0].value)?;
    if object.is_object() {
        // S4 methods get the promises too, as GNU R's `R_possible_dispatch` does, and an S4
        // object without one falls back to S3 methods of its class.
        let value = if object.has_gp(gp::S4_OBJECT)
            && let Some(value) = s4_dispatch(op, call, promised, env)?
        {
            Some(value)
        } else if let Some(found) =
            find_s3_method(definition.name, object, env, values::base_namespace(), None)?
        {
            Some(apply_internal_method(call, found, promised, env)?)
        } else {
            None
        };
        if let Some(value) = value {
            interp().visible = true;
            return Ok(Some(value));
        }
    }
    for arg in promised {
        let value = if arg.value == missing_arg() || promise_of_missing(arg.value)? {
            missing_arg()
        } else {
            value_of(arg.value)?
        };
        crate::heap().root(value);
        args.push(Arg {
            tag: arg.tag,
            value,
        });
    }
    Ok(None)
}

/// Whether `value` is an unforced promise of a symbol that is a missing argument where the
/// promise was made, which the primitive takes as an empty subscript.
unsafe fn promise_of_missing(value: Sexp) -> R<bool> {
    if value.kind() != SexpType::Promise || value.car() != unbound() {
        return Ok(false);
    }
    let environment = value.tag();
    if environment.kind() != SexpType::Environment {
        return Ok(false);
    }
    eval::argument_is_missing(value.cdr(), environment)
}

unsafe fn internal_dispatch(name: &str, call: Sexp, args: &[Arg], env: Sexp) -> R<Option<Sexp>> {
    let object = args[0].value;
    let Some(found) = find_s3_method(name, object, env, values::base_namespace(), None)? else {
        return Ok(None);
    };
    let promised = promised_args(call, args)?;
    apply_internal_method(call, found, &promised, env).map(Some)
}

unsafe fn apply_internal_method(call: Sexp, found: Found, promised: &[Arg], env: Sexp) -> R<Sexp> {
    let new_call = renamed_call(call, found.method_name)?;
    // GNU R evaluates the method inside a context of the builtin's own call, which shows in
    // `sys.calls()`.
    let state = interp();
    let handlers = state.handlers.clone();
    let restarts = state.restarts.clone();
    let promargs = eval::args_pairlist(promised)?;
    state.frames.push(Frame {
        call,
        function: found.method,
        env,
        sys_parent: env,
        promargs,
        on_exit: Vec::new(),
        handlers: handlers.clone(),
        restarts: restarts.clone(),
        srcref: state.srcref,
    });
    let depth = state.frames.len();
    let result = apply_method(new_call, found.method, promised, env, &found.vars);
    let state = interp();
    // The frame's saved stack, which `.addGlobHands` may have rewritten.
    state.handlers = state.frames[depth - 1].handlers.clone();
    state.frames.truncate(depth - 1);
    state.restarts = restarts;
    result
}

unsafe fn group_dispatch(
    group: &str,
    generic: &str,
    call: Sexp,
    args: &[Arg],
    env: Sexp,
) -> R<Option<Sexp>> {
    let binary = group == "Ops" || group == "matrixOps";
    let operands = if binary { args.len().min(2) } else { 1 };
    let mut found: Vec<Option<Found>> = Vec::new();
    for arg in args.iter().take(operands) {
        if arg.value.is_object() {
            found.push(find_group_method(group, generic, arg.value, env)?);
        } else {
            found.push(None);
        }
    }
    let (_, position) = match (found.first(), found.get(1)) {
        (Some(Some(left)), Some(Some(right))) => {
            let left_name = symbol_name(left.method_name);
            let right_name = symbol_name(right.method_name);
            // GNU R lets date-time arithmetic win over `Ops.difftime`, so `time + 2 * diff`
            // shifts the time instead of warning about incompatible methods.
            let date_arithmetic = |name: &str, minus: bool| {
                name == "+.POSIXt"
                    || name == "+.Date"
                    || (minus && (name == "-.POSIXt" || name == "-.Date"))
            };
            if left.method == right.method {
                (0, 0)
            } else if right_name == "Ops.difftime" && date_arithmetic(&left_name, true) {
                (0, 0)
            } else if left_name == "Ops.difftime" && date_arithmetic(&right_name, false) {
                (1, 1)
            } else if choose_ops_method(call, args, left, right, false)? {
                (0, 0)
            } else if choose_ops_method(call, args, right, left, true)? {
                (1, 1)
            } else {
                crate::conditions::warning_call(
                    call,
                    format!(
                        "Incompatible methods (\"{left_name}\", \"{right_name}\") for \"{generic}\""
                    ),
                )?;
                return Ok(None);
            }
        }
        (Some(Some(_)), _) => (0, 0),
        (_, Some(Some(_))) => (1, 1),
        _ => return Ok(None),
    };
    let Some(found_method) = found.into_iter().nth(position).flatten() else {
        return Ok(None);
    };
    let mut vars = found_method.vars;
    if binary {
        // `.Method` names the method for each operand, with "" for the other.
        let method_name = symbol_name(found_method.method_name).into_owned();
        let methods: Vec<String> = (0..operands)
            .map(|index| {
                if index == position
                    || (operands == 2 && args[index].value.is_object() && {
                        let other_classes = implicit_class(args[index].value).ok();
                        other_classes.is_some_and(|classes| {
                            (0..length(classes)).any(|class_index| {
                                let class = char_str(classes.element(class_index));
                                method_name == format!("{generic}.{class}")
                                    || method_name == format!("{group}.{class}")
                            })
                        })
                    })
                {
                    method_name.clone()
                } else {
                    String::new()
                }
            })
            .collect();
        let method_vector = string_vector(&methods)?;
        if let Some(entry) = vars
            .iter_mut()
            .find(|(symbol, _)| *symbol == symbols().dot_method)
        {
            entry.1 = method_vector;
        }
    }
    let promised = promised_args(call, args)?;
    let new_call = renamed_call(call, found_method.method_name)?;
    apply_method(new_call, found_method.method, &promised, env, &vars).map(Some)
}

/// Asks `chooseOpsMethod(x, y, mx, my, cl, reverse)` whether `chosen`, the method found for
/// one operand, should win over `other`, the method found for the other operand. `reverse`
/// says that `chosen` belongs to the right operand.
unsafe fn choose_ops_method(
    call: Sexp,
    args: &[Arg],
    chosen: &Found,
    other: &Found,
    reverse: bool,
) -> R<bool> {
    let (Some(left), Some(right)) = (args.first(), args.get(1)) else {
        return Ok(false);
    };
    let (x, y) = if reverse {
        (right.value, left.value)
    } else {
        (left.value, right.value)
    };
    let function = eval::find_fun(
        values::install("chooseOpsMethod")?,
        values::base_namespace(),
    )?;
    let reverse = values::scalar_bool(reverse)?;
    crate::heap().root(reverse);
    let chooser_call = values::lcons(values::install("chooseOpsMethod")?, nil())?;
    crate::heap().root(chooser_call);
    let chooser_args: Vec<Arg> = [x, y, chosen.method, other.method, call, reverse]
        .into_iter()
        .map(|value| Arg { tag: nil(), value })
        .collect();
    let answer = eval::apply(chooser_call, function, &chooser_args, values::global_env())?;
    Ok(values::as_logical_scalar(answer) == 1)
}

unsafe fn find_group_method(
    group: &str,
    generic: &str,
    object: Sexp,
    env: Sexp,
) -> R<Option<Found>> {
    let classes = implicit_class(object)?;
    let def_env = values::base_namespace();
    for index in 0..length(classes) {
        let class = char_str(classes.element(index)).into_owned();
        for prefix in [generic, group] {
            let name = method_symbol(prefix, &class)?;
            if let Some(method) = lookup_method(name, env, def_env)? {
                let suffix = class_suffix(classes, index)?;
                let method_string = scalar_string(&symbol_name(name))?;
                let vars =
                    dispatch_vars(generic, suffix, method_string, env, def_env, Some(group))?;
                return Ok(Some(Found {
                    method,
                    method_name: name,
                    vars,
                }));
            }
        }
    }
    Ok(None)
}

// S4.

/// What the methods package set up for one primitive with `setGeneric` or `setMethod`.
pub struct PrimitiveMethods {
    pub state: MethodsState,
    /// The generic function, a closure.
    pub generic: Sexp,
    /// The methods table `quick_method_check` searches.
    pub methods: Sexp,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MethodsState {
    None,
    NeedsReset,
    Has,
    Suppressed,
}

/// `R_set_prim_method`: `code` is one of `"clear"`, `"reset"`, `"set"` and `"suppress"`.
pub unsafe fn set_primitive_methods(op: Sexp, code: &str, generic: Sexp, methods: Sexp) -> R<()> {
    let state = match code {
        "clear" => MethodsState::None,
        "reset" => MethodsState::NeedsReset,
        "set" => MethodsState::Has,
        "suppress" => MethodsState::Suppressed,
        _ => {
            return Err(error(format!(
                "invalid primitive methods code (\"{code}\"): should be \"clear\", \"reset\", \"set\", or \"suppress\""
            )));
        }
    };
    if !matches!(op.kind(), SexpType::Builtin | SexpType::Special) {
        return Err(error("invalid object: must be a primitive function"));
    }
    let table = &mut interp().globals.primitive_methods;
    let index = op.primitive_index();
    match state {
        MethodsState::None => {
            table.remove(&index);
        }
        MethodsState::Suppressed => {
            if let Some(entry) = table.get_mut(&index) {
                entry.state = state;
            }
        }
        MethodsState::Has | MethodsState::NeedsReset => {
            if generic != nil() && generic.kind() != SexpType::Closure {
                return Err(error(format!(
                    "the formal definition of a primitive generic must be a function object (got type '{}')",
                    generic.kind().name()
                )));
            }
            let entry = table.entry(index).or_insert(PrimitiveMethods {
                state,
                generic: nil(),
                methods: nil(),
            });
            entry.state = state;
            if generic != nil() {
                entry.generic = generic;
            }
            if methods != nil() {
                entry.methods = methods;
            }
        }
    }
    Ok(())
}

/// The generic and methods table the methods package set for primitive `op`, if any.
pub unsafe fn primitive_methods(op: Sexp) -> Option<(Sexp, Sexp)> {
    if !matches!(op.kind(), SexpType::Builtin | SexpType::Special) {
        return None;
    }
    interp()
        .globals
        .primitive_methods
        .get(&op.primitive_index())
        .filter(|entry| entry.state != MethodsState::None)
        .map(|entry| (entry.generic, entry.methods))
}

/// `R_possible_dispatch`: dispatches primitive `op` through the methods package when it made
/// the primitive generic. `None` means the primitive's own code runs.
pub unsafe fn s4_dispatch(op: Sexp, call: Sexp, args: &[Arg], env: Sexp) -> R<Option<Sexp>> {
    if !interp().globals.allow_primitive_methods {
        return Ok(None);
    }
    let Some(entry) = interp()
        .globals
        .primitive_methods
        .get(&op.primitive_index())
    else {
        return Ok(None);
    };
    if !matches!(entry.state, MethodsState::Has | MethodsState::NeedsReset) {
        return Ok(None);
    }
    let (generic, methods) = (entry.generic, entry.methods);
    if generic.kind() != SexpType::Closure {
        return Ok(None);
    }
    let promised = promised_args(call, args)?;
    if methods != nil() {
        let arguments = eval::args_pairlist(&promised)?;
        let method = crate::methods::quick_method_check(arguments, methods, generic)?;
        match method.kind() {
            // The default method of an internal generic such as `as.vector` stands for the
            // internal code itself; calling it would dispatch again.
            SexpType::Closure if values::inherits(method, "internalDispatchMethod") => {
                return Ok(None);
            }
            SexpType::Closure => {
                let value = apply_closure(call, method, &promised, env, None)?;
                return Ok(Some(value));
            }
            // The table holds the primitive itself where its own code is the method.
            SexpType::Builtin | SexpType::Special => return Ok(None),
            _ => {}
        }
    }
    let value = apply_closure(call, generic, &promised, env, None)?;
    if value == deferred_default_marker()? {
        return Ok(None);
    }
    Ok(Some(value))
}

/// The value a generic returns to have the primitive's own code run instead.
pub unsafe fn deferred_default_marker() -> R<Sexp> {
    values::install("__Deferred_Default_Marker__")
}

unsafe fn do_is_methods_dispatch_on(_call: &Call<'_>) -> R<Sexp> {
    values::scalar_bool(interp().globals.methods_dispatch)
}

/// `standardGeneric("f")`: hands the call to the dispatcher the methods package installed.
pub unsafe fn do_standard_generic(call: &Call<'_>) -> R<Sexp> {
    if !interp().globals.methods_dispatch {
        return Err(call
            .error("standardGeneric called without 'methods' dispatch enabled (will be ignored)"));
    }
    let name = call.arg(0);
    if name.kind() != SexpType::String || length(name) != 1 {
        return Err(
            call.error("argument to 'standardGeneric' must be a non-empty character string")
        );
    }
    let env = call.env;
    let generic = this_generic(name)?;
    if generic == nil() {
        return Err(call.error(format!(
            "call to standardGeneric(\"{}\") apparently not from the body of that generic function",
            values::string_at(name, 0).unwrap_or_default()
        )));
    }
    crate::methods::dispatch_generic(name, env, generic)
}

/// The innermost function on the call stack whose `generic` attribute names `name`, which is
/// how `standardGeneric` finds the generic it was called from.
unsafe fn this_generic(name: Sexp) -> R<Sexp> {
    let wanted = values::string_at(name, 0).unwrap_or_default().into_owned();
    let generic_symbol = values::install("generic")?;
    for frame in interp().frames.iter().rev() {
        let function = frame.function;
        if function.kind() != SexpType::Closure {
            continue;
        }
        let attribute = get_attr(function, generic_symbol);
        if values::string_at(attribute, 0).is_some_and(|text| *text == wanted) {
            return Ok(function);
        }
    }
    Ok(nil())
}

unsafe fn do_set_s4_object(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let flag = values::as_logical_scalar(call.arg(1)) == 1;
    let result = if values::maybe_shared(object) {
        values::shallow_duplicate(object)?
    } else {
        object
    };
    result.set_gp_bits(gp::S4_OBJECT, flag);
    Ok(result)
}

// The class builtins.

unsafe fn do_class(call: &Call<'_>) -> R<Sexp> {
    class_of(call.arg(0))
}

unsafe fn do_old_class(call: &Call<'_>) -> R<Sexp> {
    Ok(get_attr(call.arg(0), symbols().class))
}

unsafe fn modifiable(object: Sexp) -> R<Sexp> {
    if values::maybe_shared(object) {
        values::shallow_duplicate(object)
    } else {
        Ok(object)
    }
}

unsafe fn do_set_old_class(call: &Call<'_>) -> R<Sexp> {
    let object = modifiable(call.arg(0))?;
    let value = call.arg(1);
    if value != nil() && value.kind() != SexpType::String {
        return Err(call.error("attempt to set invalid 'class' attribute"));
    }
    set_class_attribute(object, value)?;
    Ok(object)
}

/// Sets the class attribute, keeping the object bit in step.
pub unsafe fn set_class_attribute(object: Sexp, value: Sexp) -> R<()> {
    if value == nil() || length(value) == 0 {
        values::remove_attr(object, symbols().class);
        object.set_object(false);
    } else {
        values::set_attr(object, symbols().class, value)?;
        object.set_object(true);
    }
    Ok(())
}

unsafe fn do_set_class(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let value = call.arg(1);
    if object == nil() {
        // Removing the class of NULL is a no-op, which `arima` relies on.
        if value == nil() {
            return Ok(nil());
        }
        return Err(call.error("attempt to set an attribute on NULL"));
    }
    let object = modifiable(object)?;
    if value == nil() || length(value) == 0 {
        set_class_attribute(object, nil())?;
        return Ok(object);
    }
    if value.kind() != SexpType::String {
        return Err(call.error("attempt to set invalid 'class' attribute"));
    }
    if length(value) == 1 {
        let class = char_str(value.element(0));
        // Setting an implicit class removes the attribute, or coerces.
        match class.as_ref() {
            "numeric" => {
                set_class_attribute(object, nil())?;
                if !matches!(object.kind(), SexpType::Integer | SexpType::Real) {
                    return crate::coerce::coerce_vector(object, SexpType::Real);
                }
                return Ok(object);
            }
            "matrix" => {
                if values::dims(object).is_none_or(|dims| dims.len() != 2) {
                    return Err(call.error(format!(
                        "invalid to set the class to matrix unless the dimension attribute is of length 2 (was {})",
                        values::dims(object).map_or(0, |dims| dims.len())
                    )));
                }
                set_class_attribute(object, nil())?;
                return Ok(object);
            }
            "array" => {
                if values::dims(object).is_none() {
                    return Err(call.error(
                        "cannot set class to \"array\" unless the dimension attribute has length > 0",
                    ));
                }
                set_class_attribute(object, nil())?;
                return Ok(object);
            }
            "function" if values::is_function(object) => {
                set_class_attribute(object, nil())?;
                return Ok(object);
            }
            name if name == object.kind().name() && object.kind() != SexpType::Real => {
                set_class_attribute(object, nil())?;
                return Ok(object);
            }
            "integer" | "double" | "character" | "logical" | "complex" | "list" | "raw" => {
                set_class_attribute(object, nil())?;
                let kind = match class.as_ref() {
                    "integer" => SexpType::Integer,
                    "double" => SexpType::Real,
                    "character" => SexpType::String,
                    "logical" => SexpType::Logical,
                    "complex" => SexpType::Complex,
                    "raw" => SexpType::Raw,
                    _ => SexpType::List,
                };
                return crate::coerce::coerce_vector(object, kind);
            }
            _ => {}
        }
    }
    set_class_attribute(object, value)?;
    Ok(object)
}

unsafe fn do_unclass(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    if values::class_attr(object) == nil() {
        return Ok(object);
    }
    if object.kind() == SexpType::Environment || object.kind() == SexpType::ExternalPtr {
        return Err(call.error(format!(
            "cannot unclass an {}",
            if object.kind() == SexpType::Environment {
                "environment"
            } else {
                "external pointer"
            }
        )));
    }
    // Unlike a replacement function, unclass may not reuse a value any variable still holds:
    // `lapply(unclass(x), f, oldClass(x))` reads the class of `x` afterwards.
    let copy = if object.refcount() > 0 {
        values::shallow_duplicate(object)?
    } else {
        object
    };
    set_class_attribute(copy, nil())?;
    Ok(copy)
}

unsafe fn do_inherits(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let what = call.arg(1);
    let which = values::as_logical_scalar(call.arg(2)) == 1;
    let classes = if object.has_gp(gp::S4_OBJECT) {
        s4_class_with_superclasses(object)?
    } else {
        implicit_class(object)?
    };
    let classes = if values::class_attr(object) == nil() {
        class_of(object).and_then(|class| {
            let mut all: Vec<String> = (0..length(class))
                .map(|index| char_str(class.element(index)).into_owned())
                .collect();
            let implicit = implicit_class(object)?;
            for index in 0..length(implicit) {
                let name = char_str(implicit.element(index)).into_owned();
                if !all.contains(&name) {
                    all.push(name);
                }
            }
            string_vector(&all)
        })?
    } else {
        classes
    };
    // A class generator (an R6 or S4 generator, say) names its class through `nameOfClass()`.
    let what = if what.kind() == SexpType::String {
        what
    } else {
        let generic = values::install("nameOfClass")?;
        let quoted = values::call(symbols().quote, &[what])?;
        let name_call = values::call(generic, &[quoted])?;
        let name = eval::eval(name_call, values::base_namespace())?;
        if name.kind() != SexpType::String {
            return Err(call.error(
                "'what' must be a character vector or an object with a nameOfClass() method",
            ));
        }
        name
    };
    let class_names: Vec<&[u8]> = (0..length(classes))
        .map(|index| classes.element(index).bytes())
        .collect();
    if which {
        let result = values::alloc(SexpType::Integer, length(what))?;
        for index in 0..length(what) {
            let wanted = what.element(index).bytes();
            values::ints_mut(result)[index] = class_names
                .iter()
                .position(|class| *class == wanted)
                .map_or(0, |position| position as i32 + 1);
        }
        return Ok(result);
    }
    let found = (0..length(what)).any(|index| class_names.contains(&what.element(index).bytes()));
    values::scalar_bool(found)
}

/// For an S4 object, its class and every class it extends, as `is()` would list them.
unsafe fn s4_class_with_superclasses(object: Sexp) -> R<Sexp> {
    let class = values::class_attr(object);
    let methods = interp().globals.methods_namespace;
    if methods == nil() || class == nil() {
        return implicit_class(object);
    }
    let extends = values::install(".extendsForS3")?;
    let function = eval::find_var(extends, methods)?;
    if function == unbound() {
        return implicit_class(object);
    }
    let function = value_of(function)?;
    let call = values::call(function, &[class])?;
    eval::eval(call, values::base_env())
}
