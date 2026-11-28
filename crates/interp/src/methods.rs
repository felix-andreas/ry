//! The methods package's native routines: S4 dispatch, slots and class support.
//!
//! The methods package selects methods in R (`.InheritForDispatch` and the tables it caches
//! into) and leaves the per-call fast path to native code: compute the dispatch label from the
//! classes of the generic's signature arguments, look it up in the generic's table of all
//! methods, and run the method in place of the generic. That path, and the small helpers the
//! package's R code reaches through `.Call`, are here.

use {
    crate::{
        builtin_libraries::NativeFn,
        conditions::{self, Handler},
        dispatch::{self, MethodsState},
        eval,
        handles::{
            self, Binding, Chars, Closure, Cx, Environment, Expression, Gc, Held, Kind, List,
            Local, NativeArgs, Pairlist, Pointer, Promise, Str, StringFill, Symbol, Tagged, Value,
            safe,
        },
        interp,
        unwind::{R, Unwind, error},
        values::symbols,
    },
    heap::{
        Marker,
        activity::{self, Activity, Counter},
        object::{Sexp, SexpType, gp},
    },
    rustc_hash::{FxHashMap, FxHashSet},
    smallvec::SmallVec,
    std::borrow::Cow,
};

pub const ROUTINES: &[(&str, NativeFn)] = &[
    (
        "R_M_setPrimitiveMethods",
        safe!(native set_primitive_methods_routine),
    ),
    (
        "R_clear_method_selection",
        safe!(native clear_method_selection),
    ),
    ("R_dummy_extern_place", safe!(native dummy_extern_place)),
    ("R_el_named", safe!(native element_named)),
    (
        "R_externalptr_prototype_object",
        safe!(native external_pointer_prototype),
    ),
    ("R_getClassFromCache", safe!(native class_from_cache)),
    ("R_getGeneric", safe!(native get_generic_routine)),
    ("R_get_slot", safe!(native get_slot)),
    ("R_hasSlot", safe!(native has_slot_routine)),
    ("R_identC", safe!(native identical_strings)),
    ("R_initMethodDispatch", safe!(native init_method_dispatch)),
    ("R_methodsPackageMetaName", safe!(native package_meta_name)),
    ("R_methods_test_MAKE_CLASS", safe!(native test_make_class)),
    ("R_methods_test_NEW", safe!(native test_new)),
    ("R_missingArg", safe!(native missing_arg_routine)),
    ("R_nextMethodCall", safe!(native next_method_call)),
    (
        "R_quick_method_check",
        safe!(native quick_method_check_routine),
    ),
    ("R_selectMethod", safe!(native select_method)),
    ("R_set_el_named", safe!(native set_element_named)),
    ("R_set_slot", safe!(native set_slot)),
    ("R_standardGeneric", safe!(native standard_generic)),
    ("do_substitute_direct", safe!(native substitute_direct)),
    ("Rf_allocS4Object", safe!(native alloc_s4_object)),
    ("R_set_method_dispatch", safe!(native set_method_dispatch)),
    ("R_get_primname", safe!(native primitive_name)),
    ("new_object", safe!(native new_object_routine)),
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
    pub force: Held<Closure>,
}

/// Registers `definitions`, replacing any registered under the same key.
pub fn defer_definitions(definitions: DeferredDefinitions) {
    drop_deferred_definitions(&definitions.key);
    crate::heap().preserve(definitions.force.raw());
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
        crate::heap().release(dropped.force.raw());
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
pub fn force_deferred_unit(context: &mut Cx<'_>, key: &str, generic: Option<&[u8]>) -> R<()> {
    let deferred = &interp().globals.deferred_definitions;
    let Some(position) = deferred
        .iter()
        .position(|definitions| definitions.key == key)
    else {
        return Ok(());
    };
    let lazy = generic.filter(|generic| deferred[position].lazy.contains(*generic));
    run_unit(context, position, lazy)
}

/// `R_dispatchGeneric`: selects and runs the method of generic `generic` named `name` for the
/// arguments in the generic's frame `env`.
pub fn dispatch_generic<'s>(
    context: &mut Cx<'s>,
    name: Local<'s>,
    env: Local<'s>,
    generic: Local<'s>,
) -> R<Local<'s>> {
    activity::count(Counter::S4Dispatches);
    let selecting = activity::enter(Activity::S4Dispatch, 0);
    let Some(generic) = generic.downcast::<Closure>() else {
        let class = dispatch::class_of(context, generic)?;
        return Err(error(format!(
            "expected a generic function or a primitive for dispatch, got an object of class \"{}\"",
            first_text(context, class)
        )));
    };
    let Some(env) = env.downcast::<Environment>() else {
        return Err(error("invalid environment for the generic function"));
    };
    // A package loader may hold back methods for this generic, which must be in its tables
    // before it selects one.
    if !interp().globals.deferred_definitions.is_empty() {
        let generic_name = first_text(context, name);
        force_deferred(context, generic_name.as_bytes(), Lookup::Generic)?;
    }
    let generic_env = context.root(generic.read(context).environment());
    let cached = valid_site(context, generic.read(context)).map(|site| site.signature.clone());
    let mut method = match cached {
        Some(signature) => {
            let values = signature_values(context, &signature, env, name)?;
            let classes = class_keys(context, &signature, &values)?;
            match cached_method(context, generic.read(context), &classes) {
                Some(method) => method,
                None => {
                    select_method_for(context, generic, generic_env, &signature, &values, &classes)?
                }
            }
        }
        None => {
            let signature = signature_symbols(context, generic_env)?;
            let values = signature_values(context, &signature, env, name)?;
            let classes = class_keys(context, &signature, &values)?;
            select_method_for(context, generic, generic_env, &signature, &values, &classes)?
        }
    };
    if handles::is_object(method) {
        method = load_method(context, method, name, env)?;
    }
    drop(selecting);
    match method.r_type() {
        SexpType::Closure if !handles::inherits(context, method, "internalDispatchMethod") => {
            let Some(method) = method.downcast::<Closure>() else {
                return Err(error("invalid object (non-function) used as method"));
            };
            exec_method(context, method, env)
        }
        // A primitive is never a method; it is the default of a primitive made generic, and
        // the marker tells the primitive to run its own code.
        SexpType::Closure | SexpType::Builtin | SexpType::Special => {
            Ok(handles::permanent_symbol(|symbols| symbols.deferred_default_marker).value())
        }
        _ => Err(error("invalid object (non-function) used as method")),
    }
}

/// A method [`select_in_slots`] found in the generic's site, with what
/// [`eval::enter_method`] takes to run it on the generic's slots. The site holds the method
/// and the values of `vars`.
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
pub(crate) fn select_in_slots(
    context: &mut Cx<'_>,
    name: Local<'_>,
    position: usize,
) -> R<Option<Selected>> {
    let state = interp();
    if position + 1 != state.frames.len() || !state.globals.deferred_definitions.is_empty() {
        return Ok(None);
    }
    // SAFETY: the generic's frame holds its function until the call returns, after the
    // caller's scope ends.
    let generic = unsafe {
        handles::held(context, handles::live_frame(context, position).function().raw())
    };
    let Some(mut site) = valid_site(context, handles::live_frame(context, position).function())
    else {
        return Ok(None);
    };
    // `standardGeneric` dispatches for the innermost generic on the stack that `name` names,
    // which is this frame's function when its `generic` attribute is that name.
    let attribute = handles::get_attribute(context, generic, handles::symbol(|s| s.generic));
    let names_generic = match (attribute.standard::<Str>(), name.standard::<Str>()) {
        (Some(attribute), Some(name)) => {
            !attribute.is_empty()
                && !name.is_empty()
                && handles::same_text(attribute.get(0), name.read(context).get(0))
        }
        _ => false,
    };
    if site.positions.is_empty() || !names_generic {
        return Ok(None);
    }
    activity::count(Counter::S4Dispatches);
    let _selecting = activity::enter(Activity::S4Dispatch, 0);
    // Every argument is forced before any class is read, as `dispatch_generic` does: forcing
    // runs code that may define classes and methods.
    let mut arguments: SmallVec<[Local<'_>; 2]> = SmallVec::new();
    let mut index = 0;
    while let Some(&place) = site.positions.get(index) {
        let place = place as usize;
        let frame = handles::live_frame(context, position);
        let read = if frame.is_missing_formal(place) {
            Some(handles::missing_argument().read(context))
        } else {
            let value = frame.slot(place);
            match value.downcast::<Promise>() {
                None => Some(value),
                Some(promise) if !handles::is_unbound(promise.result()) => Some(promise.result()),
                Some(promise) => eval::settle_without_code(context, promise),
            }
        };
        let value = match read {
            // SAFETY: the generic's frame holds the value in its slot, itself or as the value
            // of the promise there, until the call returns, after the caller's scope ends.
            // Only the frame's own code assigns its slots while no environment stands for it,
            // and the loop gives up once forcing an argument makes one.
            Some(value) => unsafe { handles::held(context, value.raw()) },
            None => {
                let frame = handles::live_frame(context, position);
                let promise = context.root(frame.slot(place));
                let symbol = site.signature[index];
                let forced = catching_errors(context, handles::null(), |context| {
                    handles::force(context, promise)
                })?;
                let value = match forced {
                    Ok(value) => value,
                    Err(message) => {
                        let generic_name = first_text(context, name);
                        return Err(argument_error(context, symbol, &generic_name, &message));
                    }
                };
                // The argument's code may have defined a method or left the frame with an
                // environment, so the site and the frame are read again.
                if position + 1 != interp().frames.len()
                    || handles::live_frame(context, position)
                        .environment()
                        .is_some()
                {
                    return Ok(None);
                }
                match valid_site(context, handles::live_frame(context, position).function()) {
                    Some(again) if again.positions.len() > index => site = again,
                    _ => return Ok(None),
                }
                value
            }
        };
        arguments.push(value);
        index += 1;
    }
    let classes = class_identities(context, &site.signature, &arguments)?;
    let Some(entry) = site.entries.iter().find(|entry| {
        entry.classes.len() == classes.len()
            && entry
                .classes
                .iter()
                .zip(&classes)
                .all(|(held, &class)| held.raw() == class)
    }) else {
        return Ok(None);
    };
    if !entry.plain
        || !entry
            .cell
            .get(context)
            .content()
            .is(entry.method.get(context))
    {
        return Ok(None);
    }
    Ok(Some(Selected {
        method: entry.method.raw(),
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
    env: Held<Environment>,
    /// The `.SigArgs`, `.SigLength` and `.AllMTable` bindings in the generic's environment and
    /// the values they held.
    bindings: [(Held<Binding>, Held<Value>); 3],
    /// The generic's own `.Generic` binding and the value it held, when it has one.
    name: Option<(Held<Binding>, Held<Value>)>,
    signature: Signature,
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
    classes: SmallVec<[Held<Chars>; 2]>,
    cell: Held<Binding>,
    method: Held<Value>,
    /// Whether the method can run on the generic's arguments as they are: it is a closure
    /// with the generic's formals, and loading it binds nothing but `vars`
    /// ([`plain_method_vars`]).
    plain: bool,
    /// The dispatch variables a plain method's frame binds.
    vars: MethodVars,
}

/// A generic's signature arguments that take part in dispatch, which are symbols and so
/// never freed.
type Signature = SmallVec<[Gc<'static, Symbol>; 2]>;

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
    marker.mark(site.env.raw());
    for (cell, value) in site.bindings.iter().chain(&site.name) {
        marker.mark(cell.raw());
        marker.mark(value.raw());
    }
    for entry in &site.entries {
        for class in &entry.classes {
            marker.mark(class.raw());
        }
        marker.mark(entry.cell.raw());
        marker.mark(entry.method.raw());
        for &(_, value) in &entry.vars {
            marker.mark(value);
        }
    }
}

/// `value` held by the S4 cache.
fn hold<T: Kind>(value: impl Pointer<T>) -> Held<T> {
    // SAFETY: what the cache holds is stored in a site, which `trace_s4_cache` marks.
    unsafe { Held::adopt(value.raw()) }
}

/// The site of `generic`, when the bindings it was made from hold what they held.
fn valid_site(context: &Cx<'_>, generic: Gc<'_, Closure>) -> Option<&'static S4Site> {
    let site = interp().method_cache.s4.sites.get(&generic.raw())?;
    (site.env.raw() == generic.environment().raw()
        && site
            .bindings
            .iter()
            .chain(&site.name)
            .all(|(cell, value)| cell.get(context).content().is(value.get(context))))
    .then_some(site)
}

fn cached_method<'s>(
    context: &Cx<'s>,
    generic: Gc<'_, Closure>,
    classes: &[Local<'_, Chars>],
) -> Option<Local<'s>> {
    let site = interp().method_cache.s4.sites.get(&generic.raw())?;
    let entry = site
        .entries
        .iter()
        .find(|entry| same_classes(&entry.classes, classes))?;
    let method = entry.method.get(context);
    entry
        .cell
        .get(context)
        .content()
        .is(method)
        .then(|| context.root(method))
}

fn same_classes(held: &[Held<Chars>], classes: &[Local<'_, Chars>]) -> bool {
    held.len() == classes.len()
        && held
            .iter()
            .zip(classes)
            .all(|(held, class)| held.raw() == class.raw())
}

/// The values of the signature arguments in the generic's frame `env`, the missing marker for
/// one the call did not supply, and for `...` the class `.dotsClass` gives it.
fn signature_values<'s>(
    context: &mut Cx<'s>,
    signature: &[Gc<'static, Symbol>],
    env: Local<'s, Environment>,
    name: Local<'s>,
) -> R<SmallVec<[Local<'s>; 2]>> {
    let dots = handles::symbol(|symbols| symbols.dots);
    let mut values = SmallVec::new();
    for &symbol in signature {
        let value = if symbol.is(dots) {
            dots_class(context, env)?
        } else if bound_missing(context, symbol, env)? {
            handles::missing_argument()
        } else {
            evaluate_argument(context, symbol, env, name)?
        };
        values.push(value);
    }
    Ok(values)
}

/// The class each signature argument contributes to the method's label: its value's first
/// class, `"missing"` for a missing argument, and for `...` the class `.dotsClass` gave.
fn class_keys<'s>(
    context: &mut Cx<'s>,
    signature: &[Gc<'static, Symbol>],
    argument_values: &[Local<'s>],
) -> R<SmallVec<[Local<'s, Chars>; 2]>> {
    let dots = handles::symbol(|symbols| symbols.dots);
    let mut classes = SmallVec::new();
    for (&symbol, &value) in signature.iter().zip(argument_values) {
        let class = if handles::is_missing_argument(value) {
            context.root(handles::symbol(|symbols| symbols.missing).name())
        } else {
            let class = if symbol.is(dots) {
                value
            } else {
                dispatch::class_of(context, value)?
            };
            match handles::dense::<Str>(context, class)? {
                Some(class) if !class.is_empty() => context.root(class.get(context, 0)),
                _ if symbol.is(dots) => return Err(error("'.dotsClass' did not return a class")),
                _ => return Err(error("internal error: an object has no class")),
            }
        };
        classes.push(class);
    }
    Ok(classes)
}

/// The first class of each signature argument as [`class_keys`] gives it, as the address of
/// the string alone: a site's entries compare classes by identity, and their own strings are
/// held, so an address read here can match only a string that is alive. Nothing is rooted,
/// which keeps the dispatch fast path free of roots.
fn class_identities<'s>(
    context: &mut Cx<'s>,
    signature: &[Gc<'static, Symbol>],
    argument_values: &[Local<'s>],
) -> R<SmallVec<[Sexp; 2]>> {
    let dots = handles::symbol(|symbols| symbols.dots);
    let mut classes = SmallVec::new();
    for (&symbol, &value) in signature.iter().zip(argument_values) {
        let class = if handles::is_missing_argument(value) {
            handles::symbol(|symbols| symbols.missing).name().raw()
        } else if symbol.is(dots) {
            match value.standard::<Str>() {
                Some(class) if !class.is_empty() => class.read(context).get(0).raw(),
                _ => return class_keys(context, signature, argument_values).map(raw_keys),
            }
        } else {
            match dispatch::class_read(context, value)?.standard::<Str>() {
                Some(class) if !class.is_empty() => class.get(0).raw(),
                _ => return class_keys(context, signature, argument_values).map(raw_keys),
            }
        };
        classes.push(class);
    }
    Ok(classes)
}

fn raw_keys(classes: SmallVec<[Local<'_, Chars>; 2]>) -> SmallVec<[Sexp; 2]> {
    classes.iter().map(|class| class.raw()).collect()
}

/// Selects the method of `generic` for the signature arguments' `values` and classes, from its
/// methods table or by inheritance, and refreshes the generic's site.
fn select_method_for<'s>(
    context: &mut Cx<'s>,
    generic: Local<'s, Closure>,
    generic_env: Local<'s, Environment>,
    signature: &[Gc<'static, Symbol>],
    argument_values: &[Local<'s>],
    classes: &[Local<'s, Chars>],
) -> R<Local<'s>> {
    let table = all_methods_table(context, generic, generic_env)?;
    let mut label: SmallVec<[u8; 64]> = SmallVec::new();
    for (index, class) in classes.iter().enumerate() {
        if index > 0 {
            label.push(b'#');
        }
        label.extend_from_slice(class.bytes(context));
    }
    let label = handles::install_bytes(context, &label)?;
    let Some(method) = frame_forced(context, table, label)? else {
        let dots = handles::symbol(|symbols| symbols.dots);
        let mut full_classes: SmallVec<[Local<'s>; 2]> = SmallVec::new();
        for (&symbol, &value) in signature.iter().zip(argument_values) {
            let class = if handles::is_missing_argument(value) {
                handles::text_vector(context, &["missing"])?.value()
            } else if symbol.is(dots) {
                value
            } else {
                dispatch_class(context, value)?
            };
            full_classes.push(class);
        }
        return inherited_method(context, &full_classes, generic, table);
    };
    if let Some(binding) = handles::frame_binding(context, table, label)
        && !binding.is_active()
        && binding.content().is(method)
    {
        remember_method(
            context,
            generic,
            generic_env,
            signature,
            classes,
            binding,
            method,
        );
    }
    Ok(method)
}

fn remember_method(
    context: &Cx<'_>,
    generic: Local<'_, Closure>,
    generic_env: Local<'_, Environment>,
    signature: &[Gc<'static, Symbol>],
    classes: &[Local<'_, Chars>],
    cell: Gc<'_, Binding>,
    method: Local<'_>,
) {
    let mut bindings: SmallVec<[(Gc<'_, Binding>, Gc<'_>); 3]> = SmallVec::new();
    for symbol in [
        handles::symbol(|symbols| symbols.sig_args),
        handles::symbol(|symbols| symbols.sig_length),
        handles::symbol(|symbols| symbols.all_mtable),
    ] {
        let Some(binding) = handles::frame_binding(context, generic_env, symbol) else {
            return;
        };
        if binding.is_active() {
            return;
        }
        bindings.push((binding, binding.content()));
    }
    let name = handles::frame_binding(
        context,
        generic_env,
        handles::symbol(|symbols| symbols.dot_generic),
    )
    .filter(|binding| !binding.is_active())
    .map(|binding| (binding, binding.content()));
    // Classes that are not interned strings would never compare equal.
    if !classes
        .iter()
        .all(|class| class.read(context).has_mark(gp::CACHED))
    {
        return;
    }
    let method_cache = &mut interp().method_cache;
    method_cache.note_written(dispatch::Written::S4Site(generic.raw()));
    let cache = &mut method_cache.s4;
    if cache.sites.len() >= MAX_S4_SITES {
        cache.sites.clear();
    }
    let held_bindings = || {
        [0, 1, 2].map(|index| {
            let (binding, value) = bindings[index];
            (hold(binding), hold(value))
        })
    };
    let site = cache.sites.entry(generic.raw()).or_insert_with(|| S4Site {
        env: hold(generic_env),
        bindings: held_bindings(),
        name: name.map(|(binding, value)| (hold(binding), hold(value))),
        signature: Signature::new(),
        positions: SmallVec::new(),
        standard: false,
        entries: SmallVec::new(),
    });
    let same_pair = |held: &(Held<Binding>, Held<Value>),
                     (binding, value): (Gc<'_, Binding>, Gc<'_>)| {
        held.0.raw() == binding.raw() && held.1.raw() == value.raw()
    };
    let unchanged = site.env.raw() == generic_env.raw()
        && site
            .bindings
            .iter()
            .zip(&bindings)
            .all(|(held, &pair)| same_pair(held, pair))
        && match (&site.name, name) {
            (Some(held), Some(pair)) => same_pair(held, pair),
            (None, None) => true,
            _ => false,
        }
        && site.signature.len() == signature.len()
        && site
            .signature
            .iter()
            .zip(signature)
            .all(|(held, symbol)| held.is(*symbol));
    if !unchanged {
        site.env = hold(generic_env);
        site.bindings = held_bindings();
        site.name = name.map(|(binding, value)| (hold(binding), hold(value)));
        site.signature = signature.into();
        site.positions.clear();
        let closure = generic.read(context);
        let dots = handles::symbol(|symbols| symbols.dots);
        for &symbol in signature {
            let mut formal = closure.formals().downcast::<Pairlist>();
            let mut place = 0;
            while let Some(cell) = formal
                && !cell.tag().is(symbol)
            {
                formal = cell.rest().downcast::<Pairlist>();
                place += 1;
            }
            if formal.is_none() || symbol.is(dots) || place >= 64 {
                site.positions.clear();
                break;
            }
            site.positions.push(place as u8);
        }
        let standard_generic = handles::symbol(|symbols| symbols.standard_generic);
        site.standard = handles::closure_source(closure)
            .and_then(|body| body.downcast::<handles::Language>())
            .is_some_and(|body| {
                body.head().is(standard_generic)
                    && body.rest().downcast::<Pairlist>().is_some_and(|argument| {
                        argument.rest().is(handles::null())
                            && argument.tag().is(handles::null())
                            && argument.head().r_type() == SexpType::String
                    })
            });
        site.entries.clear();
    }
    let vars = name.and_then(|(_, name)| plain_method_vars(context, generic, method, name));
    let entry = S4Entry {
        classes: classes.iter().map(|&class| hold(class)).collect(),
        cell: hold(cell),
        method: hold(method),
        plain: vars.is_some(),
        vars: vars.unwrap_or_default(),
    };
    if let Some(stale) = site
        .entries
        .iter_mut()
        .find(|stale| same_classes(&stale.classes, classes))
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
fn plain_method_vars(
    context: &Cx<'_>,
    generic: Local<'_, Closure>,
    method: Local<'_>,
    generic_name: Gc<'_>,
) -> Option<MethodVars> {
    let closure = method.downcast::<Closure>()?;
    if handles::inherits(context, method, "internalDispatchMethod") {
        return None;
    }
    let (mut formal, mut generic_formal) = (
        closure.read(context).formals().downcast::<Pairlist>(),
        generic.read(context).formals().downcast::<Pairlist>(),
    );
    while let (Some(cell), Some(generic_cell)) = (formal, generic_formal) {
        if !cell.tag().is(generic_cell.tag()) {
            return None;
        }
        formal = cell.rest().downcast::<Pairlist>();
        generic_formal = generic_cell.rest().downcast::<Pairlist>();
    }
    if formal.is_some() || generic_formal.is_some() {
        return None;
    }
    let symbols = symbols();
    let mut vars = MethodVars::new();
    if handles::is_object(method) {
        for (tag, value) in handles::attribute_cells(context, method) {
            if tag.raw() == symbols.target {
                vars.push((symbols.dot_target, value.raw()));
            } else if tag.raw() == symbols.defined {
                vars.push((symbols.dot_defined, value.raw()));
            } else if tag.raw() != symbols.generic
                && tag.raw() != symbols.class
                && tag.raw() != symbols.srcref
            {
                return None;
            }
        }
        vars.push((symbols.dot_method, method.raw()));
    }
    if generic_name.r_type() == SexpType::Promise {
        return None;
    }
    vars.push((symbols.dot_generic, generic_name.raw()));
    Some(vars)
}

/// `R_quick_dispatch`: the method already in the table of generic `generic` for the classes of
/// `arguments` (a pairlist of the primitive's matched arguments), or `NULL` when the generic
/// itself must select one.
pub fn quick_method_check<'s>(
    context: &mut Cx<'s>,
    arguments: Local<'s>,
    methods: Local<'s>,
    generic: Local<'s>,
) -> R<Local<'s>> {
    let generic_env = generic
        .downcast::<Closure>()
        .map(|generic| context.root(generic.read(context).environment()));
    let table = match generic_env {
        Some(generic_env) => eval::frame_content(
            context,
            generic_env,
            handles::symbol(|symbols| symbols.all_mtable),
        )?,
        None => None,
    };
    let Some(table) = table
        .and_then(|table| table.downcast::<Environment>())
        .or_else(|| methods.downcast::<Environment>())
    else {
        return Ok(handles::null());
    };
    let signature_length = match generic_env {
        Some(generic_env) => match eval::frame_content(
            context,
            generic_env,
            handles::symbol(|symbols| symbols.sig_length),
        )? {
            Some(value) if !handles::is_unbound(value) => {
                handles::integer_scalar(context, value).max(0) as usize
            }
            _ => 1,
        },
        None => 1,
    };
    let mut label = String::new();
    let mut argument = arguments;
    for index in 0..signature_length {
        // Arguments past the ones supplied are missing.
        let cell = argument.downcast::<Pairlist>();
        let value = match cell {
            Some(cell) => context.root(cell.read(context).head()),
            None => handles::missing_argument(),
        };
        let class = if handles::is_missing_argument(value) {
            "missing".to_owned()
        } else {
            let value = handles::force(context, value)?;
            let class = dispatch_class(context, value)?;
            first_text(context, class)
        };
        if index > 0 {
            label.push('#');
        }
        label.push_str(&class);
        if let Some(cell) = cell {
            argument = context.root(cell.read(context).rest());
        }
    }
    let label = handles::install_bytes(context, label.as_bytes())?;
    match eval::frame_content(context, table, label)? {
        Some(method) if !handles::is_unbound(method) => handles::force(context, method),
        _ => Ok(handles::null()),
    }
}

/// The primitive's arguments `args` reordered into the signature of generic `generic`, as a
/// pairlist holding one value per signature argument (the missing marker where none was
/// supplied), for `quick_method_check`. GNU R reads the cache key straight off the supplied
/// arguments in order, so `x[i, drop = FALSE]` takes the class of `drop` for `j` and reuses
/// the method cached for `x[i, j]`; matching by name first keys it as `j` missing. `None`
/// means the arguments need full matching (a partial or unknown name), which the generic does.
pub fn signature_arguments<'s>(
    context: &mut Cx<'s>,
    args: &[Tagged<'s>],
    generic: Local<'s>,
) -> R<Option<Local<'s>>> {
    let Some(generic) = generic.downcast::<Closure>() else {
        return Ok(None);
    };
    let generic_env = context.root(generic.read(context).environment());
    for symbol in [
        handles::symbol(|symbols| symbols.sig_args),
        handles::symbol(|symbols| symbols.sig_length),
    ] {
        match eval::frame_content(context, generic_env, symbol)? {
            Some(value) if !handles::is_unbound(value) => {}
            _ => return Ok(None),
        }
    }
    let signature = signature_symbols(context, generic_env)?;
    let mut formals: SmallVec<[Gc<'static, Symbol>; 8]> = SmallVec::new();
    let mut formal = generic.read(context).formals().downcast::<Pairlist>();
    while let Some(cell) = formal {
        let Some(tag) = cell.tag().downcast::<Symbol>() else {
            return Ok(None);
        };
        formals.push(handles::permanent(tag));
        formal = cell.rest().downcast::<Pairlist>();
    }
    let dots = handles::symbol(|symbols| symbols.dots);
    let mut matched: SmallVec<[Local<'s>; 8]> =
        std::iter::repeat_n(handles::missing_argument(), formals.len()).collect();
    let mut taken: SmallVec<[bool; 8]> = std::iter::repeat_n(false, formals.len()).collect();
    for arg in args.iter().filter(|arg| !arg.tag.is(handles::null())) {
        let Some(index) = formals.iter().position(|formal| arg.tag.is(*formal)) else {
            return Ok(None);
        };
        if formals[index].is(dots) || taken[index] {
            return Ok(None);
        }
        matched[index] = arg.value;
        taken[index] = true;
    }
    let mut next = 0;
    for arg in args.iter().filter(|arg| arg.tag.is(handles::null())) {
        while next < formals.len() && taken[next] && !formals[next].is(dots) {
            next += 1;
        }
        // Positional arguments past `...` belong to the dots, which no signature names.
        if next >= formals.len() || formals[next].is(dots) {
            break;
        }
        matched[next] = arg.value;
        taken[next] = true;
    }
    let mut list = handles::null();
    for symbol in signature.iter().rev() {
        let value = formals
            .iter()
            .position(|formal| formal.is(*symbol))
            .and_then(|index| matched.get(index).copied())
            .unwrap_or_else(handles::missing_argument);
        list = handles::cons(context, handles::null(), value, list)?;
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
pub fn exec_method<'s>(
    context: &mut Cx<'s>,
    method: Local<'s, Closure>,
    generic_env: Local<'s, Environment>,
) -> R<Local<'s>> {
    let position = interp()
        .frames
        .iter()
        .rposition(|frame| frame.owns(generic_env.raw()))
        .ok_or_else(|| error("could not find the generic function's frame"))?;
    let call = context.root(handles::live_frame(context, position).call());
    let sys_parent =
        handles::frame_sys_parent(context, position, eval::Reify::Site(eval::Site::S4Dispatch))?;
    eval::detach_frame_references(context, position)?;
    let arguments: SmallVec<[Tagged<'s>; 8]> = handles::live_frame(context, position)
        .arguments()
        .map(|(tag, value)| Tagged {
            tag: handles::root_unless_permanent(context, tag),
            value: context.root(value),
        })
        .collect();
    let enclosure = context.root(method.read(context).environment());
    let method_env = handles::new_environment(context, enclosure)?;
    let mut formal = context.root(method.read(context).formals());
    while let Some(cell) = formal.downcast::<Pairlist>() {
        let (tag, default) = {
            let cell = cell.read(context);
            (cell.tag().downcast::<Symbol>(), context.root(cell.head()))
        };
        let Some(symbol) = tag.map(handles::permanent) else {
            return Err(error("invalid formal argument list for the method"));
        };
        let Some(binding) = handles::frame_binding(context, generic_env, symbol) else {
            return Err(error(format!(
                "could not find symbol \"{}\" in environment of the generic function",
                handles::text(symbol.name())
            )));
        };
        let missing_bit = binding.has_flag(gp::BINDING_MISSING);
        let content = binding.content();
        let missing = missing_bit || handles::is_missing_argument(content);
        let absent = binding.has_flag(gp::BINDING_ABSENT) || handles::is_missing_argument(content);
        let unforced = content
            .downcast::<Promise>()
            .is_none_or(|promise| handles::is_unbound(promise.result()));
        let binding = context.root(binding);
        let mut value = context.root(content);
        if missing_bit && !handles::is_missing_argument(value) && unforced {
            // GNU R hands the method the generic's own promise with the method's default in
            // it, so the generic's body reads the method's default after the dispatch too.
            let promise = handles::promise(context, default, method_env)?;
            context.set_binding_value(binding, promise.value())?;
            value = promise.value();
        }
        let symbol = handles::interned_symbol(symbol);
        eval::define_variable(context, symbol, value, method_env)?;
        if missing && let Some(defined) = handles::frame_binding(context, method_env, symbol) {
            handles::mark_missing(context, defined, absent);
        }
        formal = context.root(cell.read(context).rest());
    }
    for symbol in [
        handles::permanent_symbol(|symbols| symbols.dot_defined),
        handles::permanent_symbol(|symbols| symbols.dot_method),
        handles::permanent_symbol(|symbols| symbols.dot_target),
        handles::permanent_symbol(|symbols| symbols.dot_method_symbol),
    ] {
        if let Some(value) = eval::frame_content(context, generic_env, symbol)?
            && !handles::is_unbound(value)
        {
            eval::define_variable(context, symbol, value, method_env)?;
        }
    }
    let generic_symbol = handles::permanent_symbol(|symbols| symbols.dot_generic);
    let Some(generic) = eval::variable(context, generic_symbol, generic_env)?
        .filter(|generic| !handles::is_unbound(*generic))
    else {
        return Err(error(
            "generic function name not found in the environment of the generic",
        ));
    };
    eval::define_variable(context, generic_symbol, generic, method_env)?;
    eval::run_in_environment(context, call, method, method_env, sys_parent, &arguments)
}

/// `R_do_new_object`: a new object of the class `class_def` defines, a deep copy of its
/// prototype, marked as S4 when the prototype is a plain S4 object or the class comes from a
/// package. Native code may write into a slot of the object it gets without checking whether
/// the slot's value is shared, so it gets a deep copy, as GNU R makes.
pub fn new_object<'s>(context: &mut Cx<'s>, class_def: Local<'s>) -> R<Local<'s>> {
    new_object_copying(context, class_def, true)
}

/// `new_object` with a shallow copy of the prototype when `deep` is false: the object, its
/// attribute list and a list prototype's spine are new, and the slot values and list elements
/// are the prototype's. Interpreted code changes a value in place only when it holds the only
/// reference, and the prototype holds one more, so a change to a slot of the new object copies
/// that value first and the prototype never sees it. `new()` and `C_new_object` copy this way,
/// which keeps a large prototype slot from being copied for every object.
pub fn new_object_copying<'s>(
    context: &mut Cx<'s>,
    class_def: Local<'s>,
    deep: bool,
) -> R<Local<'s>> {
    if class_def.is(handles::null()) {
        return Err(error(
            "C level NEW macro called with null class definition pointer",
        ));
    }
    let virtual_flag = slot_value(context, class_def, handles::symbol(|s| s.virtual_))?;
    let class_name = slot_value(context, class_def, handles::symbol(|s| s.class_name))?;
    if handles::logical_scalar(context, virtual_flag) != 0 {
        return Err(error(format!(
            "trying to generate an object from a virtual class (\"{}\")",
            first_text(context, class_name)
        )));
    }
    let prototype = slot_value(context, class_def, handles::symbol(|s| s.prototype))?;
    let mut value = if deep {
        handles::deep_copy(context, prototype)?
    } else {
        handles::shallow_copy(context, prototype)?
    };
    // Copying an environment, a symbol or an external pointer gives the object itself, so a
    // class set here would land on the one shared object. GNU R's `R_do_new_object` skips them.
    let reference_data = matches!(
        value.get().r_type(),
        SexpType::Environment | SexpType::Symbol | SexpType::ExternalPtr
    );
    let has_package = !handles::get_attribute(context, class_name, handles::symbol(|s| s.package))
        .is(handles::null());
    if (value.get().r_type() == SexpType::Object || has_package) && !reference_data {
        handles::set_attribute(
            context,
            &mut value,
            handles::symbol(|s| s.class),
            class_name,
        )?;
        handles::mark_s4(context, &mut value);
    }
    Ok(value.finish().value())
}

/// `R_do_MAKE_CLASS`: the definition of class `name`, from the methods package's `getClass`.
pub fn make_class<'s>(context: &mut Cx<'s>, name: &str) -> R<Local<'s>> {
    let get_class = methods_function(context, "getClass")?;
    let name = handles::text_vector(context, &[name])?;
    let call = handles::call_of(context, get_class, &[name.value()])?;
    handles::evaluate(context, call.value(), handles::global_environment())
}

/// `R_has_slot`: whether `object` has slot `name`. Every object but a plain S4 one has a data
/// part.
pub fn has_slot<'s>(context: &mut Cx<'s>, object: Local<'s>, name: Local<'s>) -> R<bool> {
    let symbol = slot_symbol(context, name)?;
    if symbol.is(handles::symbol(|s| s.dot_data)) && object.r_type() != SexpType::Object {
        return Ok(true);
    }
    Ok(!handles::get_attribute(context, object, symbol).is(handles::null()))
}

/// The symbol a slot name stands for: a symbol, or a single string.
pub fn slot_symbol<'s>(context: &mut Cx<'s>, name: Local<'s>) -> R<Gc<'static, Symbol>> {
    if let Some(symbol) = name.downcast::<Symbol>() {
        return Ok(handles::permanent(symbol));
    }
    match strings(context, name)? {
        Some(text) if text.len() == 1 => handles::install_name_at(context, text, 0),
        _ => Err(error("invalid type or length for slot name")),
    }
}

/// `R_data_class(object, TRUE)`: the one class S4 dispatch uses for `object`. A class attribute
/// of length one is kept as it is, with its `package` attribute.
pub fn dispatch_class<'s>(context: &mut Cx<'s>, object: Local<'s>) -> R<Local<'s>> {
    let class = dispatch::class_of(context, object)?;
    first_class(context, class)
}

// Selection.

/// The generic's `.AllMTable`, which the methods package sets up on first use.
fn all_methods_table<'s>(
    context: &mut Cx<'s>,
    generic: Local<'s, Closure>,
    generic_env: Local<'s, Environment>,
) -> R<Local<'s, Environment>> {
    let symbol = handles::symbol(|symbols| symbols.all_mtable);
    if let Some(table) = frame_forced(context, generic_env, symbol)?
        .and_then(|table| table.downcast::<Environment>())
    {
        return Ok(table);
    }
    let get_table = methods_function(context, ".getMethodsTable")?;
    let yes = handles::logical_constant(context, 1)?;
    let call = handles::call_of(
        context,
        get_table,
        &[generic.value(), generic_env.value(), yes, yes],
    )?;
    let table = handles::evaluate(context, call.value(), handles::base_environment())?;
    table
        .downcast::<Environment>()
        .ok_or_else(|| error("invalid methods table request"))
}

/// The generic's signature arguments that take part in dispatch: the first `.SigLength` of
/// `.SigArgs`.
fn signature_symbols(context: &mut Cx<'_>, generic_env: Local<'_, Environment>) -> R<Signature> {
    let arguments = frame_forced(
        context,
        generic_env,
        handles::symbol(|symbols| symbols.sig_args),
    )?;
    let count = frame_forced(
        context,
        generic_env,
        handles::symbol(|symbols| symbols.sig_length),
    )?;
    let (Some(arguments), Some(count)) = (arguments, count) else {
        return Err(error(
            "the generic function's environment has no signature information",
        ));
    };
    let count = (handles::integer_scalar(context, count).max(0) as usize)
        .min(handles::length(context, arguments));
    let mut result = Signature::with_capacity(count);
    if let Some(names) = handles::dense::<Str>(context, arguments)? {
        for index in 0..count {
            result.push(handles::install_name_at(context, names, index)?);
        }
    } else if let Some(list) = handles::dense::<List>(context, arguments)? {
        for index in 0..count {
            let element = context.root(list.get(context, index));
            result.push(match element.downcast::<Symbol>() {
                Some(symbol) => handles::permanent(symbol),
                None => slot_symbol(context, element)?,
            });
        }
    } else {
        return Err(error(
            "invalid signature arguments in the generic function's environment",
        ));
    }
    Ok(result)
}

/// Whether the generic's own binding of `symbol` is missing. Only the binding counts: an
/// argument passed on from a caller where it is missing is not missing here, so dispatch
/// evaluates it and reports the error.
fn bound_missing(
    context: &Cx<'_>,
    symbol: Gc<'static, Symbol>,
    env: Local<'_, Environment>,
) -> R<bool> {
    let binding = handles::frame_binding(context, env, symbol).ok_or_else(|| {
        error(format!(
            "could not find symbol '{}' in frame of call",
            handles::text(symbol.name())
        ))
    })?;
    Ok(binding.has_flag(gp::BINDING_MISSING) || handles::is_missing_argument(binding.content()))
}

/// The class of `...` for dispatch, from the methods package's `.dotsClass`.
fn dots_class<'s>(context: &mut Cx<'s>, env: Local<'s, Environment>) -> R<Local<'s>> {
    let function = methods_function(context, ".dotsClass")?;
    let dots = handles::permanent_symbol(|symbols| symbols.dots);
    let call = handles::call_of(context, function, &[dots.value()])?;
    let class = handles::evaluate(context, call.value(), env)?;
    first_class(context, class)
}

fn first_class<'s>(context: &mut Cx<'s>, class: Local<'s>) -> R<Local<'s>> {
    match handles::dense::<Str>(context, class)? {
        Some(names) if names.len() > 1 => {
            let mut first = StringFill::new(context, 1)?;
            first.push(context, names.get(context, 0));
            Ok(first.finish().finish().value())
        }
        _ => Ok(class),
    }
}

/// Forces a signature argument. GNU R evaluates it under an error handler that reports which
/// argument failed, and that handler's call, `h(simpleError(msg, call))`, is the call the error
/// is reported against.
fn evaluate_argument<'s>(
    context: &mut Cx<'s>,
    symbol: Gc<'static, Symbol>,
    env: Local<'s, Environment>,
    name: Local<'s>,
) -> R<Local<'s>> {
    // A value already at hand cannot fail, so it needs no handler.
    if let Some(binding) = handles::frame_binding(context, env, symbol)
        && !binding.is_active()
    {
        let value = binding.content();
        match value.downcast::<Promise>() {
            None => return Ok(context.root(value)),
            Some(promise) if !handles::is_unbound(promise.result()) => {
                return Ok(context.root(promise.result()));
            }
            Some(_) => {}
        }
    }
    let expression = handles::interned_symbol(symbol);
    match catching_errors(context, env.value(), |context| {
        handles::evaluate(context, expression.value(), env)
    })? {
        Ok(value) => Ok(value),
        Err(message) => {
            let generic_name = first_text(context, name);
            Err(argument_error(context, symbol, &generic_name, &message))
        }
    }
}

/// The error dispatch raises when evaluating the signature argument `symbol` of the generic
/// `generic_name` failed with `message`.
fn argument_error(
    context: &mut Cx<'_>,
    symbol: Gc<'static, Symbol>,
    generic_name: &str,
    message: &str,
) -> Unwind {
    let handler_call = match handler_call(context) {
        Ok(call) => call,
        Err(unwind) => return unwind,
    };
    handles::error_in(
        handler_call,
        format!(
            "error in evaluating the argument '{}' in selecting a method for function '{generic_name}': {message}",
            handles::text(symbol.name()),
        ),
    )
}

/// `h(simpleError(msg, call))`, the call of the handler GNU R evaluates a signature argument
/// under.
fn handler_call<'s>(context: &mut Cx<'s>) -> R<Local<'s>> {
    let symbol = |context: &mut Cx<'s>, name: &[u8]| {
        handles::install_bytes(context, name).map(handles::interned_symbol)
    };
    let simple_error = symbol(context, b"simpleError")?;
    let message = symbol(context, b"msg")?;
    let call = symbol(context, b"call")?;
    let handler = symbol(context, b"h")?;
    let simple_error = handles::call_of(
        context,
        simple_error.value(),
        &[message.value(), call.value()],
    )?;
    Ok(handles::call_of(context, handler.value(), &[simple_error.value()])?.value())
}

/// Runs `evaluate` under an exiting handler for errors established from `env`: `Err` holds
/// the message of the error it caught.
fn catching_errors<'s>(
    context: &mut Cx<'s>,
    env: Local<'s>,
    evaluate: impl FnOnce(&mut Cx<'s>) -> R<Local<'s>>,
) -> R<Result<Local<'s>, String>> {
    let target = handles::new_environment(context, handles::empty_environment())?;
    let state = interp();
    let saved = state.handlers.clone();
    // The handler stack is a root, so its entries hold the class, the environment and the
    // target.
    state.handlers = Some(conditions::new_handler(Handler {
        class: handles::symbol(|symbols| symbols.error).name().raw(),
        handler: handles::null().raw(),
        calling: false,
        calling_below: conditions::calling_established(&saved),
        parent_env: env.raw(),
        target: target.raw(),
        next: saved.clone(),
    }));
    let result = evaluate(context);
    conditions::restore_handlers(saved);
    let unwind = match result {
        Ok(value) => return Ok(Ok(value)),
        Err(unwind) => unwind,
    };
    let Some(value) = handles::returned_value(context, &unwind, target.value()) else {
        return Err(unwind);
    };
    let condition = match handles::dense::<List>(context, value)? {
        Some(list) if !list.is_empty() => context.root(list.get(context, 0)),
        _ => handles::null(),
    };
    if condition.is(handles::null()) {
        Ok(Err(interp().error_message.clone()))
    } else {
        Ok(Err(conditions::condition_message(context, condition)?))
    }
}

/// `.InheritForDispatch(classes, fdef, mtable)`: selects an inherited method and caches it in
/// the table. The call names the function by value, as GNU R's does, which shows in its errors.
fn inherited_method<'s>(
    context: &mut Cx<'s>,
    classes: &[Local<'s>],
    generic: Local<'s, Closure>,
    table: Local<'s, Environment>,
) -> R<Local<'s>> {
    let raw_classes: SmallVec<[Sexp; 2]> = classes.iter().map(|class| class.raw()).collect();
    // SAFETY: the classes, the generic and the table are rooted, and the method found is rooted
    // before anything else allocates.
    let found = unsafe {
        crate::methods_core::inherited_method(&raw_classes, generic.raw(), table.raw())?
            .map(|method| handles::adopt(context, method))
    };
    if let Some(method) = found {
        return Ok(method);
    }
    let function = methods_function(context, ".InheritForDispatch")?;
    let mut list = context.alloc::<List>(classes.len())?;
    for (index, &class) in classes.iter().enumerate() {
        list.set(context, index, class);
    }
    let list = list.finish();
    let call = handles::call_of(
        context,
        function,
        &[list.value(), generic.value(), table.value()],
    )?;
    handles::evaluate(context, call.value(), handles::base_environment())
}

/// `R_loadMethod`: binds the dispatch variables of method definition `method` in the generic's
/// frame. A plain `MethodDefinition` is handled here; any other class of method goes through
/// the methods package's `loadMethod` generic, except when loading a method of `loadMethod`
/// itself, which would recurse.
fn load_method<'s>(
    context: &mut Cx<'s>,
    method: Local<'s>,
    name: Local<'s>,
    env: Local<'s, Environment>,
) -> R<Local<'s>> {
    let symbols = symbols();
    let mut extra = false;
    let mut bound: SmallVec<[(Local<'s, Symbol>, Local<'s>); 2]> = SmallVec::new();
    for (tag, value) in handles::attribute_cells(context, method) {
        if tag.raw() == symbols.target {
            bound.push((
                handles::permanent_symbol(|symbols| symbols.dot_target),
                context.root(value),
            ));
        } else if tag.raw() == symbols.defined {
            bound.push((
                handles::permanent_symbol(|symbols| symbols.dot_defined),
                context.root(value),
            ));
        } else if tag.raw() != symbols.generic
            && tag.raw() != symbols.class
            && tag.raw() != symbols.srcref
        {
            extra = true;
        }
    }
    for (symbol, value) in bound {
        eval::define_variable(context, symbol, value, env)?;
    }
    let dot_method = handles::permanent_symbol(|symbols| symbols.dot_method);
    eval::define_variable(context, dot_method, method, env)?;
    if !extra || first_text(context, name) == "loadMethod" {
        return Ok(method);
    }
    let function = methods_function(context, "loadMethod")?;
    let call = handles::call_of(context, function, &[method, name, env.value()])?;
    let namespace = methods_namespace(context)?;
    handles::evaluate(context, call.value(), namespace)
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
pub(crate) fn force_deferred(context: &mut Cx<'_>, name: &[u8], lookup: Lookup<'_>) -> R<bool> {
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
            run_unit(context, position, lazy.then_some(name))?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// Marks the unit of the definitions at `position` as run, and the first unit with it, then
/// calls their `force` for it unless both ran already. Definitions with no unit left are
/// unregistered.
fn run_unit(context: &mut Cx<'_>, position: usize, lazy: Option<&[u8]>) -> R<()> {
    let deferred = &mut interp().globals.deferred_definitions;
    let Some(definitions) = deferred.get_mut(position) else {
        return Ok(());
    };
    let first = std::mem::replace(&mut definitions.first_pending, false);
    let lazy = lazy.filter(|generic| definitions.lazy.remove(*generic));
    if !first && lazy.is_none() {
        return Ok(());
    }
    // `force` stays preserved while it runs, as a loader's registration holds it: an entry
    // with no unit left hands its preserve over, and any other may be replaced during the call.
    let force = context.root(definitions.force.get(context));
    if definitions.lazy.is_empty() {
        deferred.remove(position);
    } else {
        crate::heap().preserve(force.raw());
    }
    let result = run_force(context, force, lazy);
    crate::heap().release(force.raw());
    result
}

fn run_force(context: &mut Cx<'_>, force: Local<'_, Closure>, lazy: Option<&[u8]>) -> R<()> {
    let argument = match lazy {
        Some(generic) => {
            handles::text_vector(context, &[String::from_utf8_lossy(generic)])?.value()
        }
        None => handles::null(),
    };
    let call = handles::call_of(context, force.value(), &[argument])?;
    handles::evaluate(context, call.value(), handles::global_environment())?;
    Ok(())
}

/// What `R_getClassFromCache(class, table)` returns for a string `class` or a class definition.
pub(crate) fn cached_class_definition<'s>(
    context: &mut Cx<'s>,
    class: Local<'s>,
    table: Local<'s, Environment>,
) -> R<Local<'s>> {
    if class.r_type() != SexpType::String {
        return Ok(class);
    }
    let Some(names) = handles::dense::<Str>(context, class)? else {
        return Ok(class);
    };
    if names.is_empty() {
        return Ok(handles::null());
    }
    let symbol = handles::install_name_at(context, names, 0)?;
    let mut value = frame_forced(context, table, symbol)?;
    if value.is_none() {
        let name = names.get(context, 0).bytes().to_vec();
        if force_deferred(context, &name, Lookup::Class)? {
            value = frame_forced(context, table, symbol)?;
        }
    }
    let Some(value) = value else {
        return Ok(handles::null());
    };
    let package_symbol = handles::symbol(|symbols| symbols.package);
    let package = handles::get_attribute(context, class, package_symbol);
    let elsewhere = match package.standard::<Str>() {
        Some(package) if package.is_empty() => false,
        Some(package) => {
            let defined = handles::get_attribute(context, value, package_symbol);
            match defined.standard::<Str>() {
                Some(defined) => {
                    defined.len() == 1 && defined.get(0).bytes() != package.get(0).bytes()
                }
                None if defined.r_type() == SexpType::String => {
                    defined_elsewhere(context, class, value)?
                }
                None => false,
            }
        }
        None if package.r_type() == SexpType::String => defined_elsewhere(context, class, value)?,
        None => false,
    };
    Ok(if elsewhere { handles::null() } else { value })
}

/// Whether the class definition `value` names a package other than the first that the
/// `package` attribute of `class` names, for attributes that are not both standard vectors.
#[cold]
fn defined_elsewhere(context: &mut Cx<'_>, class: Local<'_>, value: Local<'_>) -> R<bool> {
    let package_symbol = handles::symbol(|symbols| symbols.package);
    let package = context.root(handles::get_attribute(context, class, package_symbol));
    let Some(package) = handles::dense::<Str>(context, package)? else {
        return Ok(false);
    };
    let defined = context.root(handles::get_attribute(context, value, package_symbol));
    let Some(defined) = handles::dense::<Str>(context, defined)? else {
        return Ok(false);
    };
    Ok(!package.is_empty()
        && defined.len() == 1
        && defined.get(context, 0).bytes() != package.get(context, 0).bytes())
}

/// `getGeneric(name, where = env)` for a plain name: the generic visible from `env`, after
/// running whatever deferred definitions create it, or `NULL`.
pub(crate) fn visible_generic<'s>(
    context: &mut Cx<'s>,
    symbol: Gc<'static, Symbol>,
    env: Local<'s, Environment>,
) -> R<Local<'s>> {
    force_deferred(context, symbol.name().bytes(), Lookup::Generic)?;
    find_generic(context, symbol, env, "")
}

/// The first generic function bound to `symbol` from `env` outward (from `package`, unless
/// that is empty): a closure with a `generic` attribute, or the generic the methods package
/// set up for a primitive.
pub(crate) fn find_generic<'s>(
    context: &mut Cx<'s>,
    symbol: Gc<'static, Symbol>,
    env: Local<'s, Environment>,
    package: &str,
) -> R<Local<'s>> {
    let generic_symbol = handles::symbol(|symbols| symbols.generic);
    let package_symbol = handles::symbol(|symbols| symbols.package);
    let mut frame = env;
    while !frame.is(handles::empty_environment()) {
        if let Some(value) = frame_forced(context, frame, symbol)? {
            let candidate = match value.r_type() {
                SexpType::Closure
                    if !handles::get_attribute(context, value, generic_symbol)
                        .is(handles::null()) =>
                {
                    Some(value)
                }
                SexpType::Builtin | SexpType::Special => {
                    dispatch::primitive_methods(context, value)
                        .map(|(generic, _)| context.root(generic))
                        .filter(|generic| !generic.is(handles::null()))
                }
                _ => None,
            };
            if let Some(candidate) = candidate {
                let defined =
                    context.root(handles::get_attribute(context, candidate, package_symbol));
                let matches = package.is_empty()
                    || handles::first_string(context, defined)
                        .is_none_or(|defined| defined == package);
                if matches {
                    return Ok(candidate);
                }
            }
        }
        let Some(enclosure) = frame.read(context).enclosure() else {
            break;
        };
        frame = context.root(enclosure);
    }
    Ok(handles::null())
}

// The routines.

fn set_primitive_methods_routine<'s>(
    context: &mut Cx<'s>,
    arguments: &NativeArgs<'_, 's>,
) -> R<Local<'s>> {
    let (name, op, code, generic, methods) = (
        arguments.value(0),
        arguments.value(1),
        arguments.value(2),
        arguments.value(3),
        arguments.value(4),
    );
    let code_text = handles::first_string(context, code)
        .ok_or_else(|| arguments.error("argument 'code' must be a character string"))?
        .into_owned();
    let primitive_name = first_text(context, name);
    if op.is(handles::null()) && primitive_name.is_empty() {
        let globals = &mut interp().globals;
        let previous = globals.allow_primitive_methods;
        match code_text.as_bytes().first() {
            Some(b'C' | b'c') => globals.allow_primitive_methods = false,
            Some(b'S' | b's') => globals.allow_primitive_methods = true,
            _ => {}
        }
        return handles::logical_constant(context, i32::from(previous));
    }
    // An internal generic such as `as.vector` is a closure whose default method is an
    // `internalDispatchMethod`; its `internal` slot names the `.Internal` function that
    // dispatches, as in GNU R's `R_set_prim_method`.
    let op = if matches!(op.r_type(), SexpType::Builtin | SexpType::Special) {
        op
    } else if handles::is_s4(op) {
        let slot = handles::install_bytes(context, b"internal")?;
        let internal = slot_value(context, op, slot)?;
        let internal_name = first_text(context, internal);
        let symbol = handles::install_bytes(context, internal_name.as_bytes())?;
        handles::internal_function(context, handles::interned_symbol(symbol)).ok_or_else(|| {
            arguments.error(format!(
                "'internal' slot does not name an internal function: {internal_name}"
            ))
        })?
    } else {
        let primitive = handles::primitive(&primitive_name).ok_or_else(|| {
            arguments.error(format!(
                "object '{primitive_name}' is not a primitive function"
            ))
        })?;
        context.root(primitive)
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

fn external_pointer_prototype<'s>(
    context: &mut Cx<'s>,
    _arguments: &NativeArgs<'_, 's>,
) -> R<Local<'s>> {
    handles::null_external_pointer(context)
}

/// `R_getClassFromCache(class, table)`: the cached definition of the class `class` names, or
/// `NULL`. A `package` attribute on the name must match the definition's package; a class
/// definition passed in is returned as it is.
fn class_from_cache<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let (class, table) = (arguments.value(0), arguments.value(1));
    if class.r_type() != SexpType::String {
        if handles::is_s4(class) {
            return Ok(class);
        }
        return Err(
            arguments.error("class should be either a character-string name or a class definition")
        );
    }
    if handles::length(context, class) == 0 {
        return Ok(handles::null());
    }
    let Some(table) = table.downcast::<Environment>() else {
        return Err(arguments.error("invalid class table: expected an environment"));
    };
    cached_class_definition(context, class, table)
}

fn get_generic_routine<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let (name, must_find, env, package) = (
        arguments.value(0),
        arguments.value(1),
        arguments.value(2),
        arguments.value(3),
    );
    let symbol = match name.downcast::<Symbol>() {
        Some(symbol) => handles::permanent(symbol),
        None => match strings(context, name)? {
            Some(names) if !names.is_empty() => handles::install_name_at(context, names, 0)?,
            _ => {
                return Err(arguments.error(
                    "invalid generic function name: expected a character string or a name",
                ));
            }
        },
    };
    let Some(env) = env.downcast::<Environment>() else {
        return Err(arguments.error("invalid environment for the generic function"));
    };
    let package = first_text(context, package);
    force_deferred(context, symbol.name().bytes(), Lookup::Generic)?;
    let generic_name = handles::text(symbol.name());
    let generic = find_generic(context, symbol, env, &package)?;
    if generic.is(handles::null()) && handles::logical_scalar(context, must_find) == 1 {
        return Err(arguments.error(format!(
            "no generic function definition found for '{generic_name}'"
        )));
    }
    Ok(generic)
}

fn get_slot<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let symbol = slot_symbol(context, arguments.value(1))
        .map_err(|_| arguments.error("invalid type or length for slot name"))?;
    crate::subset::slot_of(
        context,
        arguments.value(0),
        handles::interned_symbol(symbol),
        Some(arguments.expression()),
    )
}

fn set_slot<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let name = arguments.value(1);
    let Some(names) = strings(context, name)?.filter(|names| names.len() == 1) else {
        return Err(arguments.error("invalid type or length for slot name"));
    };
    let symbol = handles::install_name_at(context, names, 0)?;
    crate::subset::set_slot_in_place(context, arguments.value(0), symbol, arguments.value(2))
}

fn has_slot_routine<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let found = has_slot(context, arguments.value(0), arguments.value(1))?;
    handles::logical_constant(context, i32::from(found))
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
fn init_method_dispatch<'s>(_context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let namespace = arguments.value(0);
    let globals = &mut interp().globals;
    let environment = namespace.downcast::<Environment>();
    if let Some(environment) = environment {
        globals.methods_namespace = environment.raw();
    }
    globals.methods_dispatch = true;
    if let Some(environment) = environment {
        // SAFETY: the call's arguments hold the namespace, and the globals hold it from here on.
        unsafe { crate::methods_core::install_fast_paths(environment.raw())? };
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

fn test_make_class<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let name = class_name_argument(context, arguments)?;
    make_class(context, &name)
}

fn test_new<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let name = class_name_argument(context, arguments)?;
    let class_def = make_class(context, &name)?;
    new_object(context, class_def)
}

fn class_name_argument(context: &mut Cx<'_>, arguments: &NativeArgs<'_, '_>) -> R<String> {
    handles::first_string(context, arguments.value(0))
        .map(Cow::into_owned)
        .ok_or_else(|| arguments.error("C level MAKE_CLASS macro called with NULL string pointer"))
}

fn missing_arg_routine<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let (symbol, env) = (arguments.value(0), arguments.value(1));
    let symbol = match symbol.downcast::<Symbol>() {
        Some(symbol) => handles::permanent(symbol),
        None => match strings(context, symbol)? {
            Some(names) if !names.is_empty() => handles::install_name_at(context, names, 0)?,
            _ => {
                let class = dispatch::class_of(context, symbol)?;
                return Err(arguments.error(format!(
                    "invalid symbol in checking for missing argument in method dispatch: expected a name, got an object of class \"{}\"",
                    first_text(context, class)
                )));
            }
        },
    };
    let env = if env.is(handles::null()) {
        handles::global_environment().value()
    } else {
        env
    };
    let Some(env) = env.downcast::<Environment>() else {
        let class = dispatch::class_of(context, env)?;
        return Err(arguments.error(format!(
            "invalid environment in checking for missing argument, '{}', in methods dispatch: supplied an object of class \"{}\"",
            handles::text(symbol.name()),
            first_text(context, class)
        )));
    };
    if handles::frame_binding(context, env, symbol).is_none() {
        return Err(arguments.error(format!(
            "could not find symbol '{}' in frame of call",
            handles::text(symbol.name())
        )));
    }
    let missing = eval::formal_is_missing(context, symbol, env.read(context))?;
    handles::logical_constant(context, i32::from(missing))
}

/// `R_nextMethodCall(matched_call, env)`: evaluates the method call `matched_call` (from
/// `match.call(expand.dots = FALSE)`) with `.nextMethod` as the function and every supplied
/// argument passed on as the method frame's own binding of it (`... = ...` for the dots, which
/// the call shows in errors). A primitive next method is
/// called directly, with its S4 dispatch suppressed so that it runs its own code.
fn next_method_call<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let (matched_call, env) = (arguments.value(0), arguments.value(1));
    let unassigned = || {
        arguments.error(
            "internal error in 'callNextMethod': '.nextMethod' was not assigned in the frame of the method call",
        )
    };
    let Some(env) = env.downcast::<Environment>() else {
        return Err(unassigned());
    };
    let next_symbol = handles::permanent_symbol(|symbols| symbols.dot_next_method);
    let Some(mut op) = frame_forced(context, env, next_symbol)? else {
        return Err(unassigned());
    };
    let mut primitive = matches!(op.r_type(), SexpType::Builtin | SexpType::Special);
    if !primitive && handles::inherits(context, op, "internalDispatchMethod") {
        let generic = frame_forced(context, env, handles::symbol(|symbols| symbols.dot_generic))?;
        let name = match generic {
            Some(generic) => handles::first_string(context, generic).map(Cow::into_owned),
            None => None,
        }
        .ok_or_else(|| {
            arguments.error("internal error in 'callNextMethod': '.Generic' was not assigned in the frame of the method call")
        })?;
        let function = handles::primitive(&name)
            .ok_or_else(|| arguments.error(format!("'{name}' is not a primitive function")))?;
        op = context.root(function);
        primitive = true;
    }
    let mut items: SmallVec<[Tagged<'s>; 8]> = SmallVec::new();
    let mut cell = match handles::cell_fields(matched_call.read(context)) {
        Some((_, _, rest)) => context.root(rest),
        None => handles::null(),
    };
    while let Some((value, tag, rest)) = handles::cell_fields(cell.read(context)) {
        let passed = !tag.is(handles::null()) && !handles::is_missing_argument(value);
        let tag = handles::root_unless_permanent(context, tag);
        let value = if passed { tag } else { context.root(value) };
        items.push(Tagged { tag, value });
        cell = context.root(rest);
    }
    let rest = handles::chain(context, &items, handles::null())?;
    let head = if primitive { op } else { next_symbol.value() };
    let next_call = handles::language(context, head, rest)?.value();
    if !primitive {
        return handles::evaluate(context, next_call, env);
    }
    let Some(index) = handles::primitive_index(op) else {
        return handles::evaluate(context, next_call, env);
    };
    let primitive_methods = &mut interp().globals.primitive_methods;
    let previous = primitive_methods.get(&index).map(|entry| entry.state);
    if let Some(entry) = primitive_methods.get_mut(&index) {
        entry.state = MethodsState::Suppressed;
    }
    let result = handles::evaluate(context, next_call, env);
    if let (Some(state), Some(entry)) =
        (previous, interp().globals.primitive_methods.get_mut(&index))
    {
        entry.state = state;
    }
    result
}

fn quick_method_check_routine<'s>(
    context: &mut Cx<'s>,
    arguments: &NativeArgs<'_, 's>,
) -> R<Local<'s>> {
    quick_method_check(
        context,
        arguments.value(0),
        arguments.value(1),
        arguments.value(2),
    )
}

/// `R_selectMethod(f, env, mlist, evalArgs)`: method selection in a `MethodsList`, the
/// representation of methods the package used before methods tables. Each level of the list
/// dispatches on one argument (its `argument` slot) through the named list in its `methods`
/// slot, falling back to `"ANY"`.
fn select_method<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let (env, mut methods_list, evaluate) =
        (arguments.value(1), arguments.value(2), arguments.value(3));
    let env = env.downcast::<Environment>();
    let evaluate = handles::logical_scalar(context, evaluate) == 1;
    let argument_slot = handles::install_bytes(context, b"argument")?;
    let methods_slot = handles::install_bytes(context, b"methods")?;
    loop {
        if is_function(methods_list) || methods_list.is(handles::null()) {
            return Ok(methods_list);
        }
        let argument = slot_value(context, methods_list, argument_slot)?;
        let methods = slot_value(context, methods_list, methods_slot)?;
        // An argument that is not a name, or not bound in an environment, selects as missing.
        let class = match (argument.downcast::<Symbol>(), env) {
            (Some(symbol), Some(env))
                if !bound_missing(context, handles::permanent(symbol), env).unwrap_or(true) =>
            {
                let value = if evaluate {
                    handles::evaluate(context, argument, env)?
                } else {
                    frame_forced(context, env, symbol)?.unwrap_or_else(handles::unbound)
                };
                let class = dispatch_class(context, value)?;
                first_text(context, class)
            }
            _ => "missing".to_owned(),
        };
        let mut selected = named_element(context, methods, &class)?;
        if selected.is(handles::null()) {
            selected = named_element(context, methods, "ANY")?;
        }
        methods_list = selected;
    }
}

fn standard_generic<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let (name, env, generic) = (arguments.value(0), arguments.value(1), arguments.value(2));
    dispatch_generic(context, name, env, generic)
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
                        crate::eval::define_variable(
                            context,
                            context.root(symbol),
                            value,
                            environment,
                        )?;
                    }
                }
            } else {
                let mut cell = frame;
                while let Some((value, tag, rest)) = handles::cell_fields(cell.read(context)) {
                    let (value, tag) = (context.root(value), tag.downcast::<Symbol>());
                    let tag = tag.map(|tag| context.root(tag));
                    cell = context.root(rest);
                    if let Some(symbol) = tag {
                        crate::eval::define_variable(context, symbol, value, environment)?;
                    }
                }
            }
            environment
        }
        None => return Err(arguments.error("invalid list for substitution")),
    };
    eval::substitute(context, object, environment)
}

fn alloc_s4_object<'s>(context: &mut Cx<'s>, _arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    Ok(handles::new_s4_object(context)?.finish().value())
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

fn primitive_name<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    let object = arguments.value(0);
    if !matches!(object.r_type(), SexpType::Builtin | SexpType::Special) {
        return Err(arguments.error("'R_get_primname' called on a non-primitive"));
    }
    let name =
        handles::builtin_definition(object).map_or("<unknown>", |definition| definition.name);
    Ok(handles::text_vector(context, &[name])?.value())
}

fn new_object_routine<'s>(context: &mut Cx<'s>, arguments: &NativeArgs<'_, 's>) -> R<Local<'s>> {
    new_object_copying(context, arguments.value(0), false)
}

// Helpers.

/// The slot `slot` of `object`, as `R_do_slot` reads it.
fn slot_value<'s>(
    context: &mut Cx<'s>,
    object: Local<'s>,
    slot: Gc<'static, Symbol>,
) -> R<Local<'s>> {
    crate::subset::slot_of(
        context,
        object,
        handles::interned_symbol(slot),
        Some(handles::null()),
    )
}

/// The value `symbol` has in the frame of `env` itself, its promise forced, or `None` when the
/// frame does not bind it.
fn frame_forced<'s>(
    context: &mut Cx<'s>,
    env: Local<'_, Environment>,
    symbol: impl Pointer<Symbol>,
) -> R<Option<Local<'s>>> {
    let Some(binding) = handles::frame_binding(context, env, symbol) else {
        return Ok(None);
    };
    if !binding.is_active() {
        let content = binding.content();
        let value = match content.downcast::<Promise>() {
            _ if handles::is_unbound(content) => return Ok(None),
            None => content,
            Some(promise) if !handles::is_unbound(promise.result()) => promise.result(),
            Some(_) => {
                let promise = context.root(content);
                return handles::force(context, promise).map(Some);
            }
        };
        return Ok(Some(context.root(value)));
    }
    match eval::frame_content(context, env, symbol)? {
        Some(value) if !handles::is_unbound(value) => handles::force(context, value).map(Some),
        _ => Ok(None),
    }
}

/// The element of the list `list` named `name`, or `NULL`.
fn named_element<'s>(context: &mut Cx<'s>, list: Local<'s>, name: &str) -> R<Local<'s>> {
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

/// `value` as a character vector, when it is one.
fn strings<'s>(context: &mut Cx<'s>, value: Local<'s>) -> R<Option<Local<'s, Str>>> {
    if value.r_type() != SexpType::String {
        return Ok(None);
    }
    handles::dense::<Str>(context, value)
}

/// The first string of `value` as text, empty when there is none.
fn first_text(context: &mut Cx<'_>, value: Local<'_>) -> String {
    handles::first_string(context, value)
        .unwrap_or_default()
        .into_owned()
}

fn is_function(value: Local<'_>) -> bool {
    matches!(
        value.r_type(),
        SexpType::Closure | SexpType::Builtin | SexpType::Special
    )
}

fn methods_namespace<'s>(context: &Cx<'s>) -> R<Local<'s, Environment>> {
    handles::methods_namespace(context).ok_or_else(|| error("the methods package is not loaded"))
}

/// A function of the methods namespace, looked up by name.
fn methods_function<'s>(context: &mut Cx<'s>, name: &str) -> R<Local<'s>> {
    let namespace = methods_namespace(context)?;
    let symbol = handles::install_bytes(context, name.as_bytes())?;
    match frame_forced(context, namespace, symbol)? {
        Some(value) if is_function(value) => Ok(value),
        _ => Err(error(format!(
            "could not find function \"{name}\" in the methods package"
        ))),
    }
}
