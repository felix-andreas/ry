//! The methods package's native routines: S4 dispatch, slots and class support.
//!
//! The methods package selects methods in R (`.InheritForDispatch` and the tables it caches
//! into) and leaves the per-call fast path to native code: compute the dispatch label from the
//! classes of the generic's signature arguments, look it up in the generic's table of all
//! methods, and run the method in place of the generic. That path, and the small helpers the
//! package's R code reaches through `.Call`, are here.

use {
    crate::{
        annotations::Entry,
        builtin_libraries::{NativeCall, NativeFn},
        builtins::Arg,
        conditions::{self, Handler},
        dispatch::{self, MethodsState},
        eval,
        handles::{
            self, Cx, Environment, Expression, List, Local, NativeArgs, Str, StringFill, Symbol,
            safe,
        },
        interp,
        unwind::{Jump, R, Unwind, error, error_in},
        values::{self, length, missing_arg, nil, symbol_name, symbols, unbound},
    },
    heap::{
        Marker,
        activity::{self, Activity, Counter},
        object::{Sexp, SexpType, gp},
    },
    rustc_hash::{FxHashMap, FxHashSet},
    smallvec::SmallVec,
    std::rc::Rc,
};

pub const ROUTINES: &[(&str, NativeFn)] = &[
    ("R_M_setPrimitiveMethods", set_primitive_methods_routine),
    (
        "R_clear_method_selection",
        safe!(native clear_method_selection),
    ),
    ("R_dummy_extern_place", safe!(native dummy_extern_place)),
    ("R_el_named", safe!(native element_named)),
    ("R_externalptr_prototype_object", external_pointer_prototype),
    ("R_getClassFromCache", class_from_cache),
    ("R_getGeneric", get_generic_routine),
    ("R_get_slot", get_slot),
    ("R_hasSlot", has_slot_routine),
    ("R_identC", safe!(native identical_strings)),
    ("R_initMethodDispatch", init_method_dispatch),
    ("R_methodsPackageMetaName", safe!(native package_meta_name)),
    ("R_methods_test_MAKE_CLASS", test_make_class),
    ("R_methods_test_NEW", test_new),
    ("R_missingArg", missing_arg_routine),
    ("R_nextMethodCall", next_method_call),
    ("R_quick_method_check", quick_method_check_routine),
    ("R_selectMethod", select_method),
    ("R_set_el_named", safe!(native set_element_named)),
    ("R_set_slot", set_slot),
    ("R_standardGeneric", standard_generic),
    ("do_substitute_direct", safe!(native substitute_direct)),
    ("Rf_allocS4Object", alloc_s4_object),
    ("R_set_method_dispatch", safe!(native set_method_dispatch)),
    ("R_get_primname", primitive_name),
    ("new_object", new_object_routine),
];

/// S4 definitions a package loader holds back until first use, in units. The first unit holds
/// the classes and every definition not split off: looking up one of `classes` or `generics`
/// by name, or a primitive in `primitives` dispatching on an object of one of `classes`, runs
/// it. Each of `lazy` is a generic of its own unit, which a lookup of that generic runs, after
/// the first unit if that has not run yet. `force` runs units: it is a closure of one argument,
/// `NULL` for the first unit or a generic's name for that generic's unit.
pub struct DeferredDefinitions {
    /// Identifies the definitions to [`drop_deferred_definitions`] and [`force_deferred_unit`]:
    /// one loaded package each.
    pub key: String,
    pub classes: FxHashSet<Box<[u8]>>,
    pub generics: FxHashSet<Box<[u8]>>,
    pub primitives: FxHashSet<Box<[u8]>>,
    pub lazy: FxHashSet<Box<[u8]>>,
    /// Whether the first unit has yet to run.
    pub first_pending: bool,
    /// Preserved while registered: a closure the loader defined in a frame that has returned,
    /// which nothing counted holds, so the frame would otherwise be released under it.
    pub force: Sexp,
}

/// Registers `definitions`, replacing any registered under the same key.
pub fn defer_definitions(definitions: DeferredDefinitions) {
    drop_deferred_definitions(&definitions.key);
    crate::heap().preserve(definitions.force);
    interp().globals.deferred_definitions.push(definitions);
}

/// Forgets the definitions registered under `key`, without running them.
pub fn drop_deferred_definitions(key: &str) {
    let deferred = &mut interp().globals.deferred_definitions;
    if let Some(position) = deferred
        .iter()
        .position(|definitions| definitions.key == key)
    {
        let dropped = deferred.remove(position);
        crate::heap().release(dropped.force);
    }
}

/// Whether some loader holds back definitions that may define classes or add methods to
/// other packages' generics, which every class lookup and dispatch must then account for.
pub fn first_units_pending() -> bool {
    interp()
        .globals
        .deferred_definitions
        .iter()
        .any(|definitions| definitions.first_pending)
}

/// Runs the unit of the definitions registered under `key` that `generic` names, or the
/// first unit for `None`, unless it ran already: what a placeholder the loader bound in a
/// unit's place does when code reaches it.
pub unsafe fn force_deferred_unit(key: &str, generic: Option<&[u8]>) -> R<()> {
    let deferred = &mut interp().globals.deferred_definitions;
    let Some(position) = deferred
        .iter()
        .position(|definitions| definitions.key == key)
    else {
        return Ok(());
    };
    let lazy = generic.filter(|generic| deferred[position].lazy.contains(*generic));
    run_unit(position, lazy)
}

/// `R_dispatchGeneric`: selects and runs the method of generic `generic` named `name` for the
/// arguments in the generic's frame `env`.
pub unsafe fn dispatch_generic(name: Sexp, env: Sexp, generic: Sexp) -> R<Sexp> {
    activity::count(Counter::S4Dispatches);
    let selecting = activity::enter(Activity::S4Dispatch, 0);
    let generic_name = || values::first_string(name).unwrap_or_default().into_owned();
    if generic.kind() != SexpType::Closure {
        let class = dispatch::class_of_raw(generic)?;
        return Err(error(format!(
            "expected a generic function or a primitive for dispatch, got an object of class \"{}\"",
            values::first_string(class).unwrap_or_default()
        )));
    }
    // A package loader may hold back methods for this generic, which must be in its tables
    // before it selects one.
    if !interp().globals.deferred_definitions.is_empty() {
        force_deferred(generic_name().as_bytes(), Lookup::Generic)?;
    }
    let generic_env = generic.tag();
    let mut method = match valid_site(generic).map(|site| site.signature.clone()) {
        Some(signature) => {
            let values = signature_values(&signature, env, &generic_name)?;
            let classes = class_keys(&signature, &values)?;
            match cached_method(generic, &classes) {
                Some(method) => method,
                None => select_method_for(generic, generic_env, &signature, &values, &classes)?,
            }
        }
        None => {
            let signature = signature_symbols(generic_env)?;
            let values = signature_values(&signature, env, &generic_name)?;
            let classes = class_keys(&signature, &values)?;
            select_method_for(generic, generic_env, &signature, &values, &classes)?
        }
    };
    if method.is_object() {
        method = load_method(method, name, env)?;
    }
    drop(selecting);
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

/// A method [`select_in_slots`] found in the generic's site, with what
/// [`eval::enter_method`] takes to run it on the generic's slots.
pub(crate) struct Selected {
    pub method: Sexp,
    /// The dispatch variables the method's frame binds, `.Generic` last
    /// ([`plain_method_vars`]).
    pub vars: MethodVars,
    /// [`S4Site::standard`].
    pub standard: bool,
}

/// [`dispatch_generic`]'s selection for a call of `standardGeneric(name)` from the slot frame
/// at `position` that has no environment: the classes of the signature arguments are read
/// from the frame's slots, which hold what the generic's formals hold when it dispatches.
/// `None` when the generic's frame has to have an environment ([`dispatch_generic`] then
/// selects the method): the site holds no method for these classes, the method needs more
/// than its dispatch variables ([`S4Entry::plain`]), a loader holds definitions back, or
/// forcing an argument gave the frame its environment.
pub(crate) unsafe fn select_in_slots(name: Sexp, position: usize) -> R<Option<Selected>> {
    let state = interp();
    if position + 1 != state.frames.len() || !state.globals.deferred_definitions.is_empty() {
        return Ok(None);
    }
    let (generic, header, missing) = {
        let frame = &state.frames[position];
        (frame.function.raw(), frame.slots as usize, frame.missing)
    };
    let Some(mut site) = valid_site(generic) else {
        return Ok(None);
    };
    // `standardGeneric` dispatches for the innermost generic on the stack that `name` names,
    // which is this frame's function when its `generic` attribute is that name.
    let attribute = values::get_attr(generic, symbols().generic);
    if site.positions.is_empty()
        || attribute.kind() != SexpType::String
        || length(attribute) == 0
        || !values::same_string(
            values::string_elt(attribute, 0),
            values::string_elt(name, 0),
        )
    {
        return Ok(None);
    }
    activity::count(Counter::S4Dispatches);
    let _selecting = activity::enter(Activity::S4Dispatch, 0);
    // Every argument is forced before any class is read, as `dispatch_generic` does: forcing
    // runs code that may define classes and methods.
    let mut arguments: SmallVec<[Sexp; 2]> = SmallVec::new();
    let mut index = 0;
    while let Some(&place) = site.positions.get(index) {
        let place = place as usize;
        let mut value = *crate::bytecode::register_address(header + 1 + place);
        if missing & (1 << place) != 0 {
            value = missing_arg();
        } else if value.kind() == SexpType::Promise {
            let promise = value;
            value = promise.car();
            if value == unbound() {
                value = match eval::settled_without_code(promise)? {
                    Some(value) => value,
                    None => {
                        let symbol = site.signature[index];
                        let forced = catching_errors(nil(), || eval::force_promise(promise))?;
                        let value = forced.map_err(|message| {
                            let generic_name = values::first_string(name).unwrap_or_default();
                            argument_error(symbol, &generic_name, &message)
                        })?;
                        // The argument's code may have defined a method or left the frame
                        // with an environment, so the site and the frame are read again.
                        let state = interp();
                        if position + 1 != state.frames.len()
                            || state.frames[position].env.raw() != nil()
                        {
                            return Ok(None);
                        }
                        match valid_site(generic) {
                            Some(again) if again.positions.len() > index => site = again,
                            _ => return Ok(None),
                        }
                        value
                    }
                };
            }
        }
        arguments.push(value);
        index += 1;
    }
    let classes = class_keys(&site.signature, &arguments)?;
    let Some(entry) = site
        .entries
        .iter()
        .find(|entry| entry.classes.as_slice() == classes.as_slice())
    else {
        return Ok(None);
    };
    if !entry.plain || entry.cell.cdr() != entry.method {
        return Ok(None);
    }
    Ok(Some(Selected {
        method: entry.method,
        vars: MethodVars::from_slice(&entry.vars),
        standard: site.standard,
    }))
}

/// The inline cache of S4 dispatch, one site per generic: the generic's signature and methods
/// table as they were when the site was made, and up to [`S4_ENTRIES`] methods by the classes
/// of the signature arguments.
///
/// A site is valid while the generic's `.SigArgs`, `.SigLength` and `.AllMTable` bindings hold
/// the values it saw. An entry holds the methods table's binding of the method's label and the
/// method it held, and is valid while that binding holds it: `setMethod` and `removeMethod`
/// assign or remove that binding (a removed binding's cell holds the unbound marker), so a
/// method defined, redefined or removed after the first call is seen. A method the table
/// inherits gets no entry until `.InheritForDispatch` has stored it under the label.
///
/// The cache is a root ([`trace_s4_cache`]), so the generics, environments and cells it names
/// are not freed and their addresses not reused while it holds them.
#[derive(Default)]
pub struct S4Cache {
    sites: FxHashMap<Sexp, S4Site>,
}

const S4_ENTRIES: usize = 4;
/// Generics remembered before the cache starts over.
const MAX_S4_SITES: usize = 4096;

struct S4Site {
    env: Sexp,
    /// The `.SigArgs`, `.SigLength` and `.AllMTable` bindings in the generic's environment and
    /// the values they held.
    bindings: [(Sexp, Sexp); 3],
    /// The generic's own `.Generic` binding and the value it held, when it has one.
    name: Option<(Sexp, Sexp)>,
    signature: SmallVec<[Sexp; 2]>,
    /// The place of each signature argument among the generic's formals, which is its slot in
    /// the generic's slot frame ([`select_in_slots`]). Empty when one of them is `...`, no
    /// formal, or past the formals a slot frame keeps a missing bit for, and dispatch then
    /// reads the generic's environment.
    positions: SmallVec<[u8; 2]>,
    /// Whether the generic's body is the call `standardGeneric("name")` and nothing else, so
    /// its frame's formals hold what the call bound when it dispatches.
    standard: bool,
    entries: SmallVec<[S4Entry; S4_ENTRIES]>,
}

struct S4Entry {
    /// The first class of each signature argument, an interned string, or `"missing"`.
    classes: SmallVec<[Sexp; 2]>,
    cell: Sexp,
    method: Sexp,
    /// Whether the method can run on the generic's arguments as they are: it is a closure
    /// with the generic's formals, and loading it binds nothing but `vars`
    /// ([`plain_method_vars`]).
    plain: bool,
    /// The dispatch variables a plain method's frame binds.
    vars: MethodVars,
}

type MethodVars = SmallVec<[(Sexp, Sexp); 4]>;

pub unsafe fn trace_s4_cache(cache: &S4Cache, marker: &mut Marker<'_>) {
    for (&generic, site) in &cache.sites {
        mark_s4_site(generic, site, marker);
    }
}

/// Marks the site of `generic`, if the cache still holds one.
pub(crate) unsafe fn trace_s4_site(cache: &S4Cache, generic: Sexp, marker: &mut Marker<'_>) {
    if let Some(site) = cache.sites.get(&generic) {
        mark_s4_site(generic, site, marker);
    }
}

unsafe fn mark_s4_site(generic: Sexp, site: &S4Site, marker: &mut Marker<'_>) {
    marker.mark(generic);
    marker.mark(site.env);
    for (cell, value) in site.bindings.into_iter().chain(site.name) {
        marker.mark(cell);
        marker.mark(value);
    }
    for entry in &site.entries {
        for &class in &entry.classes {
            marker.mark(class);
        }
        marker.mark(entry.cell);
        marker.mark(entry.method);
        for &(_, value) in &entry.vars {
            marker.mark(value);
        }
    }
}

/// The site of `generic`, when the bindings it was made from hold what they held.
unsafe fn valid_site(generic: Sexp) -> Option<&'static S4Site> {
    let site = interp().method_cache.s4.sites.get(&generic)?;
    (site.env == generic.tag()
        && site
            .bindings
            .iter()
            .chain(&site.name)
            .all(|&(cell, value)| cell.cdr() == value))
    .then_some(site)
}

unsafe fn cached_method(generic: Sexp, classes: &[Sexp]) -> Option<Sexp> {
    let site = interp().method_cache.s4.sites.get(&generic)?;
    let entry = site
        .entries
        .iter()
        .find(|entry| entry.classes.as_slice() == classes)?;
    (entry.cell.cdr() == entry.method).then_some(entry.method)
}

/// The values of the signature arguments in the generic's frame `env`, the missing marker for
/// one the call did not supply, and for `...` the class `.dotsClass` gives it.
unsafe fn signature_values(
    signature: &[Sexp],
    env: Sexp,
    generic_name: &dyn Fn() -> String,
) -> R<SmallVec<[Sexp; 2]>> {
    let mut values: SmallVec<[Sexp; 2]> = SmallVec::new();
    for &symbol in signature {
        let value = if symbol == symbols().dots {
            dots_class(env)?
        } else if bound_missing(symbol, env)? {
            missing_arg()
        } else {
            evaluate_argument(symbol, env, generic_name)?
        };
        crate::heap().root(value);
        values.push(value);
    }
    Ok(values)
}

/// The class each signature argument contributes to the method's label: its value's first
/// class, `"missing"` for a missing argument, and for `...` the class `.dotsClass` gave.
unsafe fn class_keys(signature: &[Sexp], argument_values: &[Sexp]) -> R<SmallVec<[Sexp; 2]>> {
    let mut classes = SmallVec::new();
    for (&symbol, &value) in signature.iter().zip(argument_values) {
        let class = if value == missing_arg() {
            symbols().missing.car()
        } else if symbol == symbols().dots {
            if value.kind() != SexpType::String || length(value) == 0 {
                return Err(error("'.dotsClass' did not return a class"));
            }
            values::string_elt(value, 0)
        } else {
            values::string_elt(dispatch::class_of_raw(value)?, 0)
        };
        classes.push(class);
    }
    Ok(classes)
}

/// Selects the method of `generic` for the signature arguments' `values` and classes, from its
/// methods table or by inheritance, and refreshes the generic's site.
unsafe fn select_method_for(
    generic: Sexp,
    generic_env: Sexp,
    signature: &[Sexp],
    argument_values: &[Sexp],
    classes: &[Sexp],
) -> R<Sexp> {
    let table = all_methods_table(generic, generic_env)?;
    let mut label: SmallVec<[u8; 64]> = SmallVec::new();
    for (index, class) in classes.iter().enumerate() {
        if index > 0 {
            label.push(b'#');
        }
        label.extend_from_slice(class.bytes());
    }
    let label = values::install_bytes(&label)?;
    let heap = crate::heap();
    let cell = heap.frame_cell(table, label);
    let method = match cell {
        Some(cell) => eval::value_of(eval::cell_value(cell)?)?,
        None => unbound(),
    };
    if method == unbound() {
        let mut full_classes = Vec::with_capacity(argument_values.len());
        for (&symbol, &value) in signature.iter().zip(argument_values) {
            let class = if value == missing_arg() {
                values::scalar_string("missing")?
            } else if symbol == symbols().dots {
                value
            } else {
                dispatch_class(value)?
            };
            heap.root(class);
            full_classes.push(class);
        }
        return inherited_method(&full_classes, generic, table);
    }
    if let Some(cell) = cell
        && !cell.has_gp(gp::ACTIVE_BINDING)
        && cell.cdr() == method
    {
        remember_method(generic, generic_env, signature, classes, cell, method);
    }
    Ok(method)
}

unsafe fn remember_method(
    generic: Sexp,
    generic_env: Sexp,
    signature: &[Sexp],
    classes: &[Sexp],
    cell: Sexp,
    method: Sexp,
) {
    let heap = crate::heap();
    let symbols = symbols();
    let mut bindings = [(nil(), nil()); 3];
    for (slot, symbol) in
        bindings
            .iter_mut()
            .zip([symbols.sig_args, symbols.sig_length, symbols.all_mtable])
    {
        let Some(binding) = heap.frame_cell(generic_env, symbol) else {
            return;
        };
        if binding.has_gp(gp::ACTIVE_BINDING) {
            return;
        }
        *slot = (binding, binding.cdr());
    }
    let name = heap
        .frame_cell(generic_env, symbols.dot_generic)
        .filter(|binding| !binding.has_gp(gp::ACTIVE_BINDING))
        .map(|binding| (binding, binding.cdr()));
    // Classes that are not interned strings would never compare equal.
    if !classes.iter().all(|class| class.has_gp(gp::CACHED)) {
        return;
    }
    let method_cache = &mut interp().method_cache;
    method_cache.note_written(dispatch::Written::S4Site(generic));
    let cache = &mut method_cache.s4;
    if cache.sites.len() >= MAX_S4_SITES {
        cache.sites.clear();
    }
    let site = cache.sites.entry(generic).or_insert_with(|| S4Site {
        env: generic_env,
        bindings,
        name,
        signature: SmallVec::new(),
        positions: SmallVec::new(),
        standard: false,
        entries: SmallVec::new(),
    });
    if site.env != generic_env
        || site.bindings != bindings
        || site.name != name
        || site.signature.as_slice() != signature
    {
        site.env = generic_env;
        site.bindings = bindings;
        site.name = name;
        site.signature = signature.into();
        site.positions.clear();
        let dots = symbols.dots;
        for &symbol in signature {
            let mut formal = generic.car();
            let mut place = 0;
            while formal.kind() == SexpType::Pairlist && formal.tag() != symbol {
                formal = formal.cdr();
                place += 1;
            }
            if formal.kind() != SexpType::Pairlist || symbol == dots || place >= 64 {
                site.positions.clear();
                break;
            }
            site.positions.push(place as u8);
        }
        let body = crate::bytecode::closure_body(generic);
        site.standard = body.kind() == SexpType::Language
            && body.car() == symbols.standard_generic
            && body.cdr().kind() == SexpType::Pairlist
            && body.cdr().cdr() == nil()
            && body.cdr().tag() == nil()
            && body.cdr().car().kind() == SexpType::String;
        site.entries.clear();
    }
    let vars = name.and_then(|(_, name)| plain_method_vars(generic, method, name));
    let entry = S4Entry {
        classes: classes.into(),
        cell,
        method,
        plain: vars.is_some(),
        vars: vars.unwrap_or_default(),
    };
    if let Some(stale) = site
        .entries
        .iter_mut()
        .find(|stale| stale.classes.as_slice() == classes)
    {
        *stale = entry;
    } else if site.entries.len() < S4_ENTRIES {
        site.entries.push(entry);
    }
}

/// The dispatch variables the frame of `method` binds when `generic`, whose `.Generic` is
/// `generic_name`, runs it on its own arguments, or `None` when running it takes more than
/// that: the method is no closure, it stands for a primitive's own code, its formals are not
/// the generic's, or it is a method definition with attributes that only the methods
/// package's `loadMethod` knows how to load ([`load_method`]).
unsafe fn plain_method_vars(generic: Sexp, method: Sexp, generic_name: Sexp) -> Option<MethodVars> {
    if method.kind() != SexpType::Closure || values::inherits(method, "internalDispatchMethod") {
        return None;
    }
    let (mut formal, mut generic_formal) = (method.car(), generic.car());
    while formal.kind() == SexpType::Pairlist && generic_formal.kind() == SexpType::Pairlist {
        if formal.tag() != generic_formal.tag() {
            return None;
        }
        formal = formal.cdr();
        generic_formal = generic_formal.cdr();
    }
    if formal.kind() == SexpType::Pairlist || generic_formal.kind() == SexpType::Pairlist {
        return None;
    }
    let symbols = symbols();
    let mut vars = MethodVars::new();
    if method.is_object() {
        let mut attribute = method.attrib();
        while attribute.kind() == SexpType::Pairlist {
            let tag = attribute.tag();
            if tag == symbols.target {
                vars.push((symbols.dot_target, attribute.car()));
            } else if tag == symbols.defined {
                vars.push((symbols.dot_defined, attribute.car()));
            } else if tag != symbols.generic && tag != symbols.class && tag != symbols.srcref {
                return None;
            }
            attribute = attribute.cdr();
        }
        vars.push((symbols.dot_method, method));
    }
    if generic_name.kind() == SexpType::Promise {
        return None;
    }
    vars.push((symbols.dot_generic, generic_name));
    Some(vars)
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

/// The primitive's arguments `args` reordered into the signature of generic `generic`, as a
/// pairlist holding one value per signature argument (the missing marker where none was
/// supplied), for `quick_method_check`. GNU R reads the cache key straight off the supplied
/// arguments in order, so `x[i, drop = FALSE]` takes the class of `drop` for `j` and reuses
/// the method cached for `x[i, j]`; matching by name first keys it as `j` missing. `None`
/// means the arguments need full matching (a partial or unknown name), which the generic does.
pub unsafe fn signature_arguments(args: &[Arg], generic: Sexp) -> R<Option<Sexp>> {
    if generic.kind() != SexpType::Closure {
        return Ok(None);
    }
    let generic_env = generic.tag();
    for name in [".SigArgs", ".SigLength"] {
        if eval::find_var_in_frame(values::install(name)?, generic_env)? == unbound() {
            return Ok(None);
        }
    }
    let signature = signature_symbols(generic_env)?;
    let mut formals = Vec::new();
    let mut formal = generic.car();
    while formal.kind() == SexpType::Pairlist {
        formals.push(formal.tag());
        formal = formal.cdr();
    }
    let mut matched = vec![missing_arg(); formals.len()];
    let mut taken = vec![false; formals.len()];
    for arg in args.iter().filter(|arg| arg.tag != nil()) {
        let Some(index) = formals.iter().position(|&formal| formal == arg.tag) else {
            return Ok(None);
        };
        if formals[index] == symbols().dots || taken[index] {
            return Ok(None);
        }
        matched[index] = arg.value;
        taken[index] = true;
    }
    let mut next = 0;
    for arg in args.iter().filter(|arg| arg.tag == nil()) {
        while next < formals.len() && taken[next] && formals[next] != symbols().dots {
            next += 1;
        }
        // Positional arguments past `...` belong to the dots, which no signature names.
        if next >= formals.len() || formals[next] == symbols().dots {
            break;
        }
        matched[next] = arg.value;
        taken[next] = true;
    }
    let mut list = nil();
    for symbol in signature.iter().rev() {
        let value = formals
            .iter()
            .position(|formal| formal == symbol)
            .and_then(|index| matched.get(index).copied())
            .unwrap_or_else(missing_arg);
        list = values::cons(value, list)?;
        crate::heap().root(list);
    }
    Ok(Some(list))
}

/// `R_execMethod`: runs `method` in place of the generic whose frame is `generic_env`. The
/// method's frame binds each formal to the generic's binding of it, so a missing argument stays
/// missing. Where the generic bound its own default and has not read it, both frames bind one
/// promise of the method's default instead, evaluated in the method's frame; a formal the generic
/// gives no default stays without a value whatever the method declares, as in GNU R. The
/// frame also gets the dispatch variables, and takes over the generic's call, caller and
/// arguments, which is what `sys.call()`, `parent.frame()` and `match.call()` see.
pub unsafe fn exec_method(method: Sexp, generic_env: Sexp) -> R<Sexp> {
    let state = interp();
    let position = state
        .frames
        .iter()
        .rposition(|frame| frame.owns(generic_env))
        .ok_or_else(|| error("could not find the generic function's frame"))?;
    let call = state.frames[position].call.raw();
    let sys_parent = eval::frame_sys_parent(position, eval::Reify::Site(eval::Site::S4Dispatch))?;
    eval::materialize_references(position)?;
    let promargs: eval::Args = crate::frame_args(state, position).into();
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
        let missing = cell.has_gp(gp::BINDING_MISSING) || cell.cdr() == missing_arg();
        let absent = cell.has_gp(gp::BINDING_ABSENT) || cell.cdr() == missing_arg();
        let mut value = cell.cdr();
        if cell.has_gp(gp::BINDING_MISSING)
            && value != missing_arg()
            && (value.kind() != SexpType::Promise || value.car() == unbound())
        {
            // GNU R hands the method the generic's own promise with the method's default in
            // it, so the generic's body reads the method's default after the dispatch too.
            value = eval::make_promise(formal.car(), method_env)?;
            heap.set_binding_value(cell, value)?;
        }
        eval::define(symbol, value, method_env)?;
        if missing && let Some(new_cell) = heap.frame_cell(method_env, symbol) {
            new_cell.set_gp_bits(gp::BINDING_MISSING, true);
            new_cell.set_gp_bits(gp::BINDING_ABSENT, absent);
        }
        formal = formal.cdr();
    }
    let symbols = symbols();
    for symbol in [
        symbols.dot_defined,
        symbols.dot_method,
        symbols.dot_target,
        symbols.dot_method_symbol,
    ] {
        let value = eval::find_var_in_frame(symbol, generic_env)?;
        if value != unbound() {
            eval::define(symbol, value, method_env)?;
        }
    }
    let generic_symbol = symbols.dot_generic;
    let generic = eval::find_var(generic_symbol, generic_env)?;
    if generic == unbound() {
        return Err(error(
            "generic function name not found in the environment of the generic",
        ));
    }
    eval::define(generic_symbol, generic, method_env)?;
    eval::run_closure(
        call,
        method,
        method_env,
        sys_parent,
        &promargs,
        Entry::Unchecked,
    )
}

/// `R_do_new_object`: a new object of the class `class_def` defines, a deep copy of its
/// prototype, marked as S4 when the prototype is a plain S4 object or the class comes from a
/// package. Native code may write into a slot of the object it gets without checking whether
/// the slot's value is shared, so it gets a deep copy, as GNU R makes.
pub unsafe fn new_object(class_def: Sexp) -> R<Sexp> {
    new_object_copying(class_def, true)
}

/// `new_object` with a shallow copy of the prototype when `deep` is false: the object, its
/// attribute list and a list prototype's spine are new, and the slot values and list elements
/// are the prototype's. Interpreted code changes a value in place only when it holds the only
/// reference, and the prototype holds one more, so a change to a slot of the new object copies
/// that value first and the prototype never sees it. `new()` and `C_new_object` copy this way,
/// which keeps a large prototype slot from being copied for every object.
pub unsafe fn new_object_copying(class_def: Sexp, deep: bool) -> R<Sexp> {
    if class_def == nil() {
        return Err(error(
            "C level NEW macro called with null class definition pointer",
        ));
    }
    let virtual_flag = crate::subset::get_slot(class_def, symbols().virtual_, nil())?;
    let class_name = crate::subset::get_slot(class_def, symbols().class_name, nil())?;
    if values::as_logical_scalar(virtual_flag) != 0 {
        return Err(error(format!(
            "trying to generate an object from a virtual class (\"{}\")",
            values::first_string(class_name).unwrap_or_default()
        )));
    }
    let prototype = crate::subset::get_slot(class_def, symbols().prototype, nil())?;
    let value = if deep {
        values::duplicate(prototype)?
    } else {
        values::shallow_duplicate(prototype)?
    };
    // Duplicating an environment, a symbol or an external pointer gives the object itself, so a
    // class set here would land on the one shared object. GNU R's `R_do_new_object` skips them.
    let reference_data = matches!(
        value.kind(),
        SexpType::Environment | SexpType::Symbol | SexpType::ExternalPtr
    );
    let package = values::get_attr(class_name, symbols().package);
    if (value.kind() == SexpType::Object || package != nil()) && !reference_data {
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
        SexpType::String if length(name) == 1 => values::install_charsxp(name.element(0)),
        _ => Err(error("invalid type or length for slot name")),
    }
}

/// `R_data_class(object, TRUE)`: the one class S4 dispatch uses for `object`. A class attribute
/// of length one is kept as it is, with its `package` attribute.
pub unsafe fn dispatch_class(object: Sexp) -> R<Sexp> {
    first_class(dispatch::class_of_raw(object)?)
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
            SexpType::String => values::install_charsxp(arguments.element(index))?,
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
    Ok(cell.has_gp(gp::BINDING_MISSING) || cell.cdr() == missing_arg())
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
unsafe fn evaluate_argument(symbol: Sexp, env: Sexp, generic_name: &dyn Fn() -> String) -> R<Sexp> {
    // A value already at hand cannot fail, so it needs no handler.
    if let Some(cell) = crate::heap().frame_cell(env, symbol)
        && !cell.has_gp(gp::ACTIVE_BINDING)
    {
        let value = cell.cdr();
        if value.kind() != SexpType::Promise {
            return Ok(value);
        }
        if value.car() != unbound() {
            return Ok(value.car());
        }
    }
    match catching_errors(env, || eval::eval(symbol, env))? {
        Ok(value) => Ok(value),
        Err(message) => Err(argument_error(symbol, &generic_name(), &message)),
    }
}

/// The error dispatch raises when evaluating the signature argument `symbol` of the generic
/// `generic_name` failed with `message`.
unsafe fn argument_error(symbol: Sexp, generic_name: &str, message: &str) -> Unwind {
    let handler_call = match handler_call() {
        Ok(call) => call,
        Err(unwind) => return unwind,
    };
    error_in(
        handler_call,
        format!(
            "error in evaluating the argument '{}' in selecting a method for function '{generic_name}': {message}",
            symbol_name(symbol),
        ),
    )
}

/// `h(simpleError(msg, call))`, the call of the handler GNU R evaluates a signature argument
/// under.
unsafe fn handler_call() -> R<Sexp> {
    let simple_error = values::call(
        values::install("simpleError")?,
        &[values::install("msg")?, values::install("call")?],
    )?;
    values::call(values::install("h")?, &[simple_error])
}

/// Runs `evaluate` under an exiting handler for errors established from `env`: `Err` holds
/// the message of the error it caught.
unsafe fn catching_errors(
    env: Sexp,
    evaluate: impl FnOnce() -> R<Sexp>,
) -> R<Result<Sexp, String>> {
    let state = interp();
    let saved = state.handlers.clone();
    let target = values::new_env(values::empty_env())?;
    state.handlers = Some(Rc::new(Handler {
        class: values::mkchar("error")?,
        handler: nil(),
        calling: false,
        calling_below: crate::conditions::calling_established(&saved),
        parent_env: env,
        target,
        next: saved.clone(),
    }));
    let result = evaluate();
    interp().handlers = saved;
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(Unwind::Jump(jump)) => match jump {
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
            other => Err(Unwind::Jump(other)),
        },
        Err(other) => Err(other),
    }
}

/// `.InheritForDispatch(classes, fdef, mtable)`: selects an inherited method and caches it in
/// the table. The call names the function by value, as GNU R's does, which shows in its errors.
unsafe fn inherited_method(classes: &[Sexp], generic: Sexp, table: Sexp) -> R<Sexp> {
    if let Some(method) = crate::methods_core::inherited_method(classes, generic, table)? {
        return Ok(method);
    }
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
        let symbols = symbols();
        if tag == symbols.target {
            eval::define(symbols.dot_target, attribute.car(), env)?;
        } else if tag == symbols.defined {
            eval::define(symbols.dot_defined, attribute.car(), env)?;
        } else if tag != symbols.generic && tag != symbols.class && tag != symbols.srcref {
            extra = true;
        }
        attribute = attribute.cdr();
    }
    eval::define(symbols().dot_method, method, env)?;
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
    // An internal generic such as `as.vector` is a closure whose default method is an
    // `internalDispatchMethod`; its `internal` slot names the `.Internal` function that
    // dispatches, as in GNU R's `R_set_prim_method`.
    let op = if matches!(op.kind(), SexpType::Builtin | SexpType::Special) {
        op
    } else if crate::coerce::is_s4(op) {
        let internal = slot_value(op, "internal")?;
        let internal_name = values::first_string(internal)
            .unwrap_or_default()
            .into_owned();
        crate::builtins::internal_function(values::install(&internal_name)?).ok_or_else(|| {
            call.error(format!(
                "'internal' slot does not name an internal function: {internal_name}"
            ))
        })?
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

fn clear_method_selection<'s>(
    _context: &mut Cx<'s>,
    _arguments: &NativeArgs<'_, 's>,
) -> R<Local<'s>> {
    Ok(handles::null())
}

fn dummy_extern_place<'s>(_context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    Err(arguments.error("calling the C routine used as an initializer for 'externalptr' objects"))
}

fn element_named<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let name = element_name(context, arguments, arguments.value(1))?;
    let list = arguments.value(0);
    let names = handles::names_of(context, list)?;
    let Some(names) = handles::dense::<Str>(context, names)? else {
        return Ok(handles::null());
    };
    let Some(index) = names
        .iter(context)
        .position(|element| element.bytes() == name.as_bytes())
    else {
        return Ok(handles::null());
    };
    if let Some(elements) = handles::dense::<List>(context, list)?
        && index < elements.len()
    {
        return Ok(context.root(elements.get(context, index)));
    }
    if let Some(expressions) = handles::dense::<Expression>(context, list)?
        && index < expressions.len()
    {
        return Ok(context.root(expressions.get(context, index)));
    }
    Ok(handles::null())
}

fn set_element_named<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    let name = element_name(context, arguments, arguments.value(1))?;
    let value = arguments.value(2);
    let Some(list) = handles::dense::<List>(context, object)? else {
        return Err(arguments.error("R_set_el_named: the object must be a list"));
    };
    let names = handles::names_of(context, object)?;
    let names = handles::dense::<Str>(context, names)?;
    let count = list.len();
    let position = names.and_then(|names| {
        names
            .iter(context)
            .take(count)
            .position(|element| element.bytes() == name.as_bytes())
    });
    if let Some(index) = position {
        let mut result = handles::shallow_duplicate(context, list)?;
        result.set(context, index, value);
        return Ok(result.finish().value());
    }
    let mut result = context.alloc::<List>(count + 1)?;
    for index in 0..count {
        result.set(context, index, list.get(context, index));
    }
    result.set(context, count, value);
    let mut result = result.owned();
    handles::copy_most_attributes(context, object, &mut result, false)?;
    let mut new_names = StringFill::new(context, count + 1)?;
    for index in 0..count {
        let text = match names {
            Some(names) if index < names.len() => {
                handles::text(names.get(context, index)).into_owned()
            }
            _ => String::new(),
        };
        new_names.push_text(context, &text)?;
    }
    new_names.push_text(context, &name)?;
    let new_names = new_names.finish().finish().value();
    handles::set_attribute(
        context,
        &mut result,
        handles::symbol(|symbols| symbols.names),
        new_names,
    )?;
    Ok(result.finish().value())
}

fn element_name<'s>(
    context: &mut Cx<'s>,
    arguments: &NativeArgs<'_, 's>,
    what: Local<'s>,
) -> R<String> {
    if let Some(what) = handles::dense::<Str>(context, what)?
        && what.len() == 1
    {
        return Ok(handles::text(what.get(context, 0)).into_owned());
    }
    if let Some(symbol) = what.downcast::<Symbol>() {
        return Ok(handles::text(symbol.read(context).name()).into_owned());
    }
    Err(arguments.error("invalid element name: expected a single string"))
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
    if class.kind() != SexpType::String && !class.has_gp(gp::S4_OBJECT) {
        return Err(
            call.error("class should be either a character-string name or a class definition")
        );
    }
    cached_class_definition(class, table)
}

/// What `R_getClassFromCache(class, table)` returns for a string `class` or a class definition.
pub(crate) unsafe fn cached_class_definition(class: Sexp, table: Sexp) -> R<Sexp> {
    if class.kind() != SexpType::String {
        return Ok(class);
    }
    if length(class) == 0 {
        return Ok(nil());
    }
    let symbol = values::install_charsxp(class.element(0))?;
    let mut value = eval::value_of(eval::find_var_in_frame(symbol, table)?)?;
    if value == unbound() && force_deferred(class.element(0).bytes(), Lookup::Class)? {
        value = eval::value_of(eval::find_var_in_frame(symbol, table)?)?;
    }
    if value == unbound() {
        return Ok(nil());
    }
    let package_symbol = symbols().package;
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

unsafe fn get_generic_routine(call: &NativeCall<'_>) -> R<Sexp> {
    let (name, must_find, env, package) = (call.arg(0), call.arg(1), call.arg(2), call.arg(3));
    let symbol = match name.kind() {
        SexpType::Symbol => name,
        SexpType::String if length(name) >= 1 => values::install_charsxp(name.element(0))?,
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
    force_deferred(symbol_name(symbol).as_bytes(), Lookup::Generic)?;
    let generic = find_generic(symbol, env, &package)?;
    if generic == nil() && values::as_logical_scalar(must_find) == 1 {
        return Err(call.error(format!(
            "no generic function definition found for '{}'",
            symbol_name(symbol)
        )));
    }
    Ok(generic)
}

/// `getGeneric(name, where = env)` for a plain name: the generic visible from `env`, after
/// running whatever deferred definitions create it, or `NULL`.
pub(crate) unsafe fn visible_generic(symbol: Sexp, env: Sexp) -> R<Sexp> {
    force_deferred(symbol_name(symbol).as_bytes(), Lookup::Generic)?;
    find_generic(symbol, env, "")
}

/// What a lookup that may need deferred definitions is after, by name.
pub(crate) enum Lookup<'a> {
    /// A class missing from the class cache.
    Class,
    Generic,
    /// A primitive about to dispatch on S4 objects of these classes.
    Primitive(&'a [&'a [u8]]),
}

/// Runs the deferred unit that the lookup of `name` needs, if any, and reports whether it ran
/// one. The unit is marked as run first, so a lookup it makes itself finds nothing deferred.
pub(crate) unsafe fn force_deferred(name: &[u8], lookup: Lookup<'_>) -> R<bool> {
    let deferred = &interp().globals.deferred_definitions;
    for (position, definitions) in deferred.iter().enumerate() {
        let first = definitions.first_pending
            && match lookup {
                Lookup::Class => definitions.classes.contains(name),
                Lookup::Generic => definitions.generics.contains(name),
                Lookup::Primitive(classes) => {
                    definitions.primitives.contains(name)
                        && classes
                            .iter()
                            .any(|class| definitions.classes.contains(*class))
                }
            };
        let lazy = matches!(lookup, Lookup::Generic) && definitions.lazy.contains(name);
        if first || lazy {
            run_unit(position, lazy.then_some(name))?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// Marks the unit of the definitions at `position` as run, and the first unit with it, then
/// calls their `force` for it unless both ran already. Definitions with no unit left are
/// unregistered.
unsafe fn run_unit(position: usize, lazy: Option<&[u8]>) -> R<()> {
    let deferred = &mut interp().globals.deferred_definitions;
    let Some(definitions) = deferred.get_mut(position) else {
        return Ok(());
    };
    let first = std::mem::replace(&mut definitions.first_pending, false);
    let lazy = lazy.filter(|generic| definitions.lazy.remove(*generic));
    if !first && lazy.is_none() {
        return Ok(());
    }
    let force = definitions.force;
    let exhausted = definitions.lazy.is_empty();
    let argument = match lazy {
        Some(generic) => values::scalar_string(&String::from_utf8_lossy(generic))?,
        None => nil(),
    };
    let heap = crate::heap();
    // The call holds `force` by a preserve of its own: an entry with no unit left is removed
    // and hands its preserve over, and any other may be replaced while the call runs.
    if exhausted {
        deferred.remove(position);
    } else {
        heap.preserve(force);
    }
    heap.preserve(argument);
    let result =
        values::call(force, &[argument]).and_then(|call| eval::eval(call, values::global_env()));
    heap.release(argument);
    heap.release(force);
    result.map(|_| ())
}

/// The first generic function bound to `symbol` from `env` outward (from `package`, unless
/// that is empty): a closure with a `generic` attribute, or the generic the methods package
/// set up for a primitive.
pub(crate) unsafe fn find_generic(symbol: Sexp, env: Sexp, package: &str) -> R<Sexp> {
    let heap = crate::heap();
    let generic_symbol = values::install("generic")?;
    let package_symbol = symbols().package;
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
    let symbol = values::install_charsxp(name.element(0))?;
    crate::subset::assign_slot(call.arg(0), symbol, call.arg(2))
}

unsafe fn has_slot_routine(call: &NativeCall<'_>) -> R<Sexp> {
    values::scalar_bool(has_slot(call.arg(0), call.arg(1))?)
}

fn identical_strings<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let left = handles::dense::<Str>(context, arguments.value(0))?;
    let right = handles::dense::<Str>(context, arguments.value(1))?;
    let same = match (left, right) {
        (Some(left), Some(right)) if left.len() == 1 && right.len() == 1 => {
            left.get(context, 0).bytes() == right.get(context, 0).bytes()
        }
        _ => false,
    };
    handles::logical_constant(context, i32::from(same))
}

/// Turns S4 dispatch on, with `where` as the methods namespace.
unsafe fn init_method_dispatch(call: &NativeCall<'_>) -> R<Sexp> {
    let namespace = call.arg(0);
    let globals = &mut interp().globals;
    if namespace.kind() == SexpType::Environment {
        globals.methods_namespace = namespace;
    }
    globals.methods_dispatch = true;
    if namespace.kind() == SexpType::Environment {
        crate::methods_core::install_fast_paths(namespace)?;
    }
    Ok(namespace)
}

/// `.__<prefix>__<name>`, with `:<package>` appended for a non-empty package: the name the
/// methods package stores its metadata objects under.
fn package_meta_name<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let mut parts = Vec::with_capacity(3);
    for (index, what) in ["prefix", "name", "package"].iter().enumerate() {
        let value = handles::dense::<Str>(context, arguments.value(index))?
            .filter(|value| value.len() == 1)
            .ok_or_else(|| {
                arguments.error(format!(
                    "The internal argument \"{what}\" to methodsPackageMetaName must be a single string"
                ))
            })?;
        parts.push(handles::text(value.get(context, 0)).into_owned());
    }
    let mut result = format!(".__{}__{}", parts[0], parts[1]);
    if !parts[2].is_empty() {
        result.push(':');
        result.push_str(&parts[2]);
    }
    let mut text = StringFill::new(context, 1)?;
    text.push_text(context, &result)?;
    Ok(text.finish().finish().value())
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
        SexpType::String if length(symbol) >= 1 => values::install_charsxp(symbol.element(0))?,
        _ => {
            let class = dispatch::class_of_raw(symbol)?;
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
        let class = dispatch::class_of_raw(env)?;
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
fn substitute_direct<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let (object, frame) = (arguments.value(0), arguments.value(1));
    let environment = match frame.downcast::<Environment>() {
        Some(environment) => environment,
        None if matches!(frame.r_type(), SexpType::List | SexpType::Pairlist) => {
            let environment = handles::new_environment(context, handles::base_environment())?;
            if let Some(list) = handles::dense::<List>(context, frame)? {
                let names = handles::names_of(context, frame)?;
                if let Some(names) = handles::dense::<Str>(context, names)? {
                    for index in 0..list.len() {
                        if names.get(context, index).bytes().is_empty() {
                            continue;
                        }
                        let symbol = handles::install_name_at(context, names, index)?;
                        let value = context.root(list.get(context, index));
                        handles::define(context, context.root(symbol), value, environment)?;
                    }
                }
            } else {
                let mut cell = frame;
                while let Some((value, tag, rest)) = handles::cell_fields(cell.read(context)) {
                    let (value, tag) = (context.root(value), tag.downcast::<Symbol>());
                    let tag = tag.map(|tag| context.root(tag));
                    cell = context.root(rest);
                    if let Some(symbol) = tag {
                        handles::define(context, symbol, value, environment)?;
                    }
                }
            }
            environment
        }
        None => return Err(arguments.error("invalid list for substitution")),
    };
    eval::substitute(context, object, environment)
}

unsafe fn alloc_s4_object(_call: &NativeCall<'_>) -> R<Sexp> {
    let object = values::triple(SexpType::Object, nil(), nil(), nil())?;
    object.set_gp_bits(gp::S4_OBJECT, true);
    Ok(object)
}

/// Reports whether table dispatch is on, which it always is here: GNU R can switch back to its
/// older `MethodsList` dispatch, but the tables are the only representation ry dispatches on.
fn set_method_dispatch<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let previous = interp().globals.methods_dispatch;
    if handles::logical_scalar(context, arguments.value(0)) == 1 {
        interp().globals.methods_dispatch = true;
    }
    handles::logical_constant(context, i32::from(previous))
}

unsafe fn primitive_name(call: &NativeCall<'_>) -> R<Sexp> {
    let object = call.arg(0);
    if !matches!(object.kind(), SexpType::Builtin | SexpType::Special) {
        return Err(call.error("'R_get_primname' called on a non-primitive"));
    }
    values::scalar_string(values::primitive_name(object))
}

unsafe fn new_object_routine(call: &NativeCall<'_>) -> R<Sexp> {
    new_object_copying(call.arg(0), false)
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
