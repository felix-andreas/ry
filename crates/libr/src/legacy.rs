//! Names R 4.6 no longer declares, which package binaries built against older R still import:
//! the true length and growable bit, frame and hash-table access, the closure and environment
//! setters, and the debugging flags. A binary that imports a missing name does not load at all,
//! so these exist for prebuilt packages, with the behavior they had.
//!
//! `TRUELENGTH` is the allocated capacity of a vector whose length was shrunk below it, which is
//! how data.table over-allocates its column lists. Code also uses it as a scratch integer on
//! strings and on vectors (data.table's grouping marks each `CHARSXP` this way and restores the
//! marks afterwards). A string keeps its mark in its own header, since those kernels touch every
//! string; a vector's scratch value lives in `Runtime::truelengths`.

use {
    crate::{
        R_xlen_t, SEXPTYPE,
        boundary::{ApiResult, enter, error},
        runtime,
        vectors::sexp_type,
    },
    heap::object::{Sexp, SexpType},
    std::ffi::c_int,
};

unsafe fn vector_or_string(object: Sexp, what: &str) -> ApiResult<Sexp> {
    if object.kind().is_vector() || object.kind() == SexpType::Char {
        Ok(object)
    } else {
        Err(error(format!(
            "{what}() can only be applied to a vector, not a '{}'",
            object.kind().name()
        )))
    }
}

pub unsafe fn truelength(object: Sexp) -> ApiResult<R_xlen_t> {
    let object = vector_or_string(object, "TRUELENGTH")?;
    if object.kind() == SexpType::Char {
        return Ok(object.char_truelength() as R_xlen_t);
    }
    if let Some(&value) = runtime().truelengths.get(&object) {
        return Ok(value);
    }
    if object.is_altrep() {
        return Ok(0);
    }
    let capacity = object.capacity();
    Ok(
        if runtime().heap.is_resizable(object) || capacity > object.length() {
            capacity
        } else {
            0
        },
    )
}

pub unsafe fn set_truelength(object: Sexp, value: R_xlen_t) -> ApiResult<()> {
    let object = vector_or_string(object, "SET_TRUELENGTH")?;
    if object.kind() == SexpType::Char {
        let value = i32::try_from(value).map_err(|_| {
            error(format!(
                "SET_TRUELENGTH() on a string takes a 32-bit value, not {value}"
            ))
        })?;
        object.set_char_truelength(value);
        return Ok(());
    }
    let runtime = runtime();
    let matches_storage = !object.is_altrep()
        && (value == object.capacity() || (value == 0 && object.capacity() == object.length()));
    if value == 0 || matches_storage {
        if runtime.truelengths.remove(&object).is_some() {
            runtime.heap.release(object);
        }
        return Ok(());
    }
    if runtime.truelengths.insert(object, value).is_none() {
        runtime.heap.preserve(object);
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn TRUELENGTH(object: Sexp) -> R_xlen_t {
    // String kernels read the truelength of every string, so a string skips the boundary.
    if object.kind() == SexpType::Char {
        return object.char_truelength() as R_xlen_t;
    }
    enter(|| truelength(object))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn XTRUELENGTH(object: Sexp) -> R_xlen_t {
    enter(|| truelength(object))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_TRUELENGTH(object: Sexp, value: R_xlen_t) {
    if object.kind() == SexpType::Char
        && let Ok(value) = i32::try_from(value)
    {
        object.set_char_truelength(value);
        return;
    }
    enter(|| set_truelength(object, value))
}

/// `SETLENGTH(x, n)`: changes the length of a vector within the length it was allocated with.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SETLENGTH(object: Sexp, length: R_xlen_t) {
    enter(|| {
        if !object.kind().is_vector() || object.kind() == SexpType::Char || object.is_altrep() {
            return Err(error(format!(
                "SETLENGTH() cannot be applied to a '{}'",
                object.kind().name()
            )));
        }
        let runtime = runtime();
        runtime.heap.mark_resizable(object);
        // data.table's finalizer sets a scratch vector's length past its allocation, only so
        // that GNU R's memory accounting sees over-allocated columns being freed, and then
        // drops the vector. Growing past the allocation would expose memory ry does not own,
        // so the length stays.
        if length > object.capacity() {
            return Ok(());
        }
        runtime.heap.resize_vector(object, length)?;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_GROWABLE_BIT(object: Sexp) {
    enter(|| {
        if object.kind().is_vector() && object.kind() != SexpType::Char && !object.is_altrep() {
            runtime().heap.mark_resizable(object);
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn IS_GROWABLE(object: Sexp) -> c_int {
    enter(|| {
        Ok((object.kind().is_vector()
            && runtime().heap.is_resizable(object)
            && object.capacity() > object.length()) as c_int)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_TYPEOF(object: Sexp, kind: SEXPTYPE) {
    enter(|| {
        let kind = sexp_type(kind)?;
        runtime().heap.retype(object, kind)?;
        Ok(())
    })
}

unsafe fn checked(object: Sexp, kind: SexpType, what: &str) -> ApiResult<Sexp> {
    if object.kind() != kind {
        return Err(error(format!(
            "{what}() can only be applied to a '{}', not a '{}'",
            kind.name(),
            object.kind().name()
        )));
    }
    Ok(object)
}

/// `FRAME(env)`: the frame as a pairlist of values tagged with their symbols. ry's frames are
/// hashed, so this is a snapshot: reading it works, and changes to it do not reach the frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn FRAME(env: Sexp) -> Sexp {
    enter(|| {
        let env = checked(env, SexpType::Environment, "FRAME")?;
        let runtime = runtime();
        let mut list = runtime.nil();
        for symbol in runtime.heap.frame_symbols(env).into_iter().rev() {
            let Some(cell) = runtime.heap.frame_cell(env, symbol) else {
                continue;
            };
            list = runtime
                .heap
                .alloc_triple(SexpType::Pairlist, cell.cdr(), list, symbol)?;
            runtime.heap.root(list);
            if cell.has_gp(heap::object::gp::ACTIVE_BINDING) {
                list.set_gp_bits(heap::object::gp::ACTIVE_BINDING, true);
            }
        }
        Ok(list)
    })
}

/// `HASHTAB(env)`: `NULL`, which tells code that walks frames to use `FRAME` instead.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn HASHTAB(env: Sexp) -> Sexp {
    enter(|| {
        checked(env, SexpType::Environment, "HASHTAB")?;
        Ok(runtime().nil())
    })
}

/// `SET_FRAME(env, frame)`: replaces every binding of `env` with the pairlist's, keeping each
/// node's active-binding mark.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_FRAME(env: Sexp, frame: Sexp) {
    enter(|| {
        let env = checked(env, SexpType::Environment, "SET_FRAME")?;
        replace_bindings(env, &[frame])
    })
}

/// `SET_HASHTAB(env, table)`: `HASHTAB` always reads `NULL` here, so a `NULL` table, which is
/// what code copying one environment's table into another passes, leaves the frame alone. A
/// list is a table built by hand, each element one bucket's pairlist of bindings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_HASHTAB(env: Sexp, table: Sexp) {
    enter(|| {
        let env = checked(env, SexpType::Environment, "SET_HASHTAB")?;
        match table.kind() {
            SexpType::Nil => Ok(()),
            SexpType::List => {
                let buckets: Vec<Sexp> = (0..table.length())
                    .map(|index| table.element(index as usize))
                    .collect();
                replace_bindings(env, &buckets)
            }
            other => Err(error(format!(
                "SET_HASHTAB() takes a list or NULL, not a '{}'",
                other.name()
            ))),
        }
    })
}

unsafe fn replace_bindings(env: Sexp, chains: &[Sexp]) -> ApiResult<()> {
    let runtime = runtime();
    for symbol in runtime.heap.frame_symbols(env) {
        runtime.heap.remove_binding(env, symbol)?;
    }
    for &chain in chains {
        let mut node = chain;
        while node.kind() == SexpType::Pairlist {
            let symbol = node.tag();
            if symbol.kind() == SexpType::Symbol {
                runtime.heap.define(env, symbol, node.car())?;
                if node.has_gp(heap::object::gp::ACTIVE_BINDING)
                    && let Some(cell) = runtime.heap.frame_cell(env, symbol)
                {
                    cell.set_gp_bits(heap::object::gp::ACTIVE_BINDING, true);
                }
            }
            node = node.cdr();
        }
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_ENCLOS(env: Sexp, enclosure: Sexp) {
    enter(|| {
        let env = checked(env, SexpType::Environment, "SET_ENCLOS")?;
        checked(enclosure, SexpType::Environment, "SET_ENCLOS")?;
        if env == runtime().heap.known().empty_env {
            return Err(error("can not set the parent of the empty environment"));
        }
        runtime().heap.set_enclosure(env, enclosure);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_FORMALS(closure: Sexp, formals: Sexp) {
    enter(|| {
        runtime()
            .heap
            .set_car(checked(closure, SexpType::Closure, "SET_FORMALS")?, formals);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_BODY(closure: Sexp, body: Sexp) {
    enter(|| {
        runtime()
            .heap
            .set_cdr(checked(closure, SexpType::Closure, "SET_BODY")?, body);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_CLOENV(closure: Sexp, env: Sexp) {
    enter(|| {
        checked(env, SexpType::Environment, "SET_CLOENV")?;
        runtime()
            .heap
            .set_tag(checked(closure, SexpType::Closure, "SET_CLOENV")?, env);
        Ok(())
    })
}

/// The debugging flags live in general-purpose bit 15 of a closure, as in GNU R.
const DEBUG_BIT: u16 = 1 << 15;
/// The single-step flag, general-purpose bit 14.
const STEP_BIT: u16 = 1 << 14;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn RDEBUG(object: Sexp) -> c_int {
    object.has_gp(DEBUG_BIT) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_RDEBUG(object: Sexp, value: c_int) {
    object.set_gp_bits(DEBUG_BIT, value != 0);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn RSTEP(object: Sexp) -> c_int {
    object.has_gp(STEP_BIT) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_RSTEP(object: Sexp, value: c_int) {
    object.set_gp_bits(STEP_BIT, value != 0);
}

/// `ENVFLAGS(env)`: an environment's general-purpose bits, where GNU R keeps its locked flag.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ENVFLAGS(env: Sexp) -> c_int {
    env.gp() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_ENVFLAGS(env: Sexp, flags: c_int) {
    env.set_gp(flags as u16);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn EXTPTR_PTR(pointer: Sexp) -> *mut std::ffi::c_void {
    enter(|| Ok(checked(pointer, SexpType::ExternalPtr, "EXTPTR_PTR")?.external_address()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn EXTPTR_TAG(pointer: Sexp) -> Sexp {
    enter(|| Ok(checked(pointer, SexpType::ExternalPtr, "EXTPTR_TAG")?.external_tag()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn EXTPTR_PROT(pointer: Sexp) -> Sexp {
    enter(|| Ok(checked(pointer, SexpType::ExternalPtr, "EXTPTR_PROT")?.external_protected()))
}

/// `SYMVALUE(symbol)`: the symbol's binding in the base environment, or `R_UnboundValue`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SYMVALUE(symbol: Sexp) -> Sexp {
    enter(|| {
        let symbol = checked(symbol, SexpType::Symbol, "SYMVALUE")?;
        let base = runtime().heap.known().base_env;
        Ok(crate::environments::frame_value(base, symbol))
    })
}

/// `INTERNAL(symbol)`: the `.Internal` function behind a symbol, which only the interpreter
/// defines, so `R_NilValue` here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn INTERNAL(symbol: Sexp) -> Sexp {
    enter(|| {
        checked(symbol, SexpType::Symbol, "INTERNAL")?;
        Ok(runtime().nil())
    })
}

/// `DDVAL(symbol)`: whether a symbol is one of `..1`, `..2`, ..., kept in general-purpose bit 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn DDVAL(symbol: Sexp) -> c_int {
    let name = symbol.car().bytes();
    (name.len() > 2 && name.starts_with(b"..") && name[2..].iter().all(u8::is_ascii_digit)) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_MISSING(cell: Sexp, value: c_int) {
    let mask: u16 = 0x000f;
    cell.set_gp((cell.gp() & !mask) | (value as u16 & mask));
}

/// `IS_SCALAR(x, type)`: whether `x` is a length-one vector of `type`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn IS_SCALAR(object: Sexp, kind: c_int) -> c_int {
    (object.kind() as c_int == kind && object.kind().is_vector() && object.length() == 1) as c_int
}

/// GNU R's internal pointer-protection stack, which packages compiled with R's internal
/// headers (the base packages among them) push onto directly through inlined macros. ry keeps
/// a real array under the exported names and scans its live part as a root.
pub const PP_STACK_SIZE: usize = 50_000;

#[unsafe(no_mangle)]
pub static R_PPStack: crate::Global<*mut Sexp> = crate::Global::new(std::ptr::null_mut());
#[unsafe(no_mangle)]
pub static R_PPStackTop: crate::Global<c_int> = crate::Global::new(0);
#[unsafe(no_mangle)]
pub static R_PPStackSize: crate::Global<c_int> = crate::Global::new(PP_STACK_SIZE as c_int);

struct PointerProtectionStack;

impl heap::RootSource for PointerProtectionStack {
    fn trace(&self, marker: &mut heap::Marker<'_>) {
        let stack = R_PPStack.get();
        let top = (R_PPStackTop.get().max(0) as usize).min(PP_STACK_SIZE);
        if stack.is_null() {
            return;
        }
        // SAFETY: the array has PP_STACK_SIZE slots, and native code stores only live handles
        // below the top.
        unsafe {
            for index in 0..top {
                marker.mark(*stack.add(index));
            }
        }
    }
}

/// Allocates the pointer-protection array and registers it as a root.
pub(crate) fn start_pointer_protection(heap: &mut heap::Heap) {
    let slots = vec![Sexp::dangling(); PP_STACK_SIZE].into_boxed_slice();
    R_PPStack.set(Box::leak(slots).as_mut_ptr());
    heap.add_root_source(std::rc::Rc::new(PointerProtectionStack));
}

/// The reference count operations GNU R exports as functions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn INCREMENT_REFCNT(object: Sexp) {
    object.increment_refcount();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn DECREMENT_REFCNT(object: Sexp) {
    object.decrement_refcount();
}

/// Named-ness follows the reference count in ry, so the `NAMED` setters have nothing to do.
#[unsafe(no_mangle)]
pub extern "C" fn ENSURE_NAMED(_object: Sexp) {}
#[unsafe(no_mangle)]
pub extern "C" fn ENSURE_NAMEDMAX(_object: Sexp) {}
#[unsafe(no_mangle)]
pub extern "C" fn RAISE_NAMED(_object: Sexp, _named: c_int) {}
#[unsafe(no_mangle)]
pub extern "C" fn SETTER_CLEAR_NAMED(_object: Sexp) {}
#[unsafe(no_mangle)]
pub extern "C" fn DISABLE_REFCNT(_object: Sexp) {}
#[unsafe(no_mangle)]
pub extern "C" fn ENABLE_REFCNT(_object: Sexp) {}

#[unsafe(no_mangle)]
pub extern "C" fn TRACKREFS(_object: Sexp) -> c_int {
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_XLENGTH(object: Sexp) -> R_xlen_t {
    enter(|| crate::vectors::xlength(object))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn XLENGTH_EX(object: Sexp) -> R_xlen_t {
    enter(|| crate::vectors::xlength(object))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn LENGTH_EX(
    object: Sexp,
    _file: *const std::ffi::c_char,
    _line: c_int,
) -> c_int {
    enter(|| Ok(crate::vectors::xlength(object)? as c_int))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn IS_LONG_VEC(object: Sexp) -> c_int {
    enter(|| Ok((crate::vectors::xlength(object)? > i32::MAX as R_xlen_t) as c_int))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn IS_CACHED(object: Sexp) -> c_int {
    object.has_gp(heap::object::gp::CACHED) as c_int
}

/// Whether a string's encoding is declared: UTF-8, Latin-1 or ASCII.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ENC_KNOWN(object: Sexp) -> c_int {
    use heap::object::gp;
    (object.has_gp(gp::UTF8) || object.has_gp(gp::LATIN1) || object.has_gp(gp::ASCII)) as c_int
}

/// The "promise seen" flag, general-purpose bit 0 of a promise, which detects recursive
/// promise evaluation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PRSEEN(promise: Sexp) -> c_int {
    (promise.gp() & 1) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_PRSEEN(promise: Sexp, value: c_int) {
    promise.set_gp((promise.gp() & !1) | (value as u16 & 1));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_PRINTNAME(symbol: Sexp, name: Sexp) {
    enter(|| {
        runtime()
            .heap
            .set_car(checked(symbol, SexpType::Symbol, "SET_PRINTNAME")?, name);
        Ok(())
    })
}

/// `MARK(x)`: the collector's mark, which is meaningful only inside a collection, and native
/// code never runs inside one.
#[unsafe(no_mangle)]
pub extern "C" fn MARK(_object: Sexp) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn RTRACE(object: Sexp) -> c_int {
    object.has_gp(1 << 13) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_RTRACE(object: Sexp, value: c_int) {
    object.set_gp_bits(1 << 13, value != 0);
}

/// Unchecked data access: GNU R's `*0` accessors skip the type check, which ry keeps.
macro_rules! data_without_check {
    ($($name:ident: $element:ty),* $(,)?) => {
        $(
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn $name(object: Sexp) -> *mut $element {
                enter(|| Ok(crate::vectors::data_pointer(object, true)? as *mut $element))
            }
        )*
    };
}

data_without_check!(
    LOGICAL0: c_int,
    INTEGER0: c_int,
    REAL0: f64,
    COMPLEX0: heap::object::Rcomplex,
    RAW0: u8,
);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn CAR0(cell: Sexp) -> Sexp {
    cell.car()
}

macro_rules! scalar_value {
    ($getter:ident, $setter:ident, $element:ty, $kind:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $getter(object: Sexp) -> $element {
            enter(|| {
                checked(object, $kind, stringify!($getter))?;
                Ok(*(crate::vectors::data_pointer(object, false)? as *const $element))
            })
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $setter(object: Sexp, value: $element) {
            enter(|| {
                checked(object, $kind, stringify!($setter))?;
                *(crate::vectors::data_pointer(object, true)? as *mut $element) = value;
                Ok(())
            })
        }
    };
}

scalar_value!(SCALAR_IVAL, SET_SCALAR_IVAL, c_int, SexpType::Integer);
scalar_value!(SCALAR_LVAL, SET_SCALAR_LVAL, c_int, SexpType::Logical);
scalar_value!(SCALAR_DVAL, SET_SCALAR_DVAL, f64, SexpType::Real);
scalar_value!(
    SCALAR_CVAL,
    SET_SCALAR_CVAL,
    heap::object::Rcomplex,
    SexpType::Complex
);
scalar_value!(SCALAR_BVAL, SET_SCALAR_BVAL, u8, SexpType::Raw);

/// `R_MB_CUR_MAX`: the longest multibyte character, which is six bytes in glibc's UTF-8.
#[unsafe(no_mangle)]
pub static R_MB_CUR_MAX: crate::Global<c_int> = crate::Global::new(6);

#[unsafe(no_mangle)]
pub static utf8locale: crate::Global<crate::Rboolean> = crate::Global::new(1);

#[unsafe(no_mangle)]
pub static known_to_be_latin1: crate::Global<crate::Rboolean> = crate::Global::new(0);

#[unsafe(no_mangle)]
pub static known_to_be_utf8: crate::Global<crate::Rboolean> = crate::Global::new(1);

/// `OutDec`: the decimal mark R prints, `options(OutDec)`.
#[unsafe(no_mangle)]
pub static OutDec: crate::Global<[std::ffi::c_char; 10]> =
    crate::Global::new([b'.' as std::ffi::c_char, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

/// `R_Visible`: whether the last top-level value prints. The interpreter owns it.
#[unsafe(no_mangle)]
pub static R_Visible: crate::Global<crate::Rboolean> = crate::Global::new(1);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_mkFalse() -> Sexp {
    enter(|| crate::memory::scalar(SexpType::Logical, 0i32))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_mkTrue() -> Sexp {
    enter(|| crate::memory::scalar(SexpType::Logical, 1i32))
}

/// `Rf_CreateTag(x)`: a symbol to tag an argument with: the symbol itself, the first string of
/// a character vector, or the deparsed expression.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_CreateTag(object: Sexp) -> Sexp {
    enter(|| {
        let runtime = runtime();
        match object.kind() {
            SexpType::Nil | SexpType::Symbol => Ok(object),
            SexpType::String if object.length() >= 1 && !object.element(0).bytes().is_empty() => {
                Ok(runtime.heap.install_char(object.element(0))?)
            }
            _ => {
                let text = crate::eval::deparse(object, 0);
                Ok(runtime.heap.install(text.as_bytes())?)
            }
        }
    })
}

/// `Rf_envlength(rho)`: the number of bindings in an environment's frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_envlength(env: Sexp) -> c_int {
    enter(|| {
        let env = checked(env, SexpType::Environment, "envlength")?;
        Ok(runtime().heap.frame_symbols(env).len() as c_int)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_envxlength(env: Sexp) -> R_xlen_t {
    enter(|| {
        let env = checked(env, SexpType::Environment, "envxlength")?;
        Ok(runtime().heap.frame_symbols(env).len() as R_xlen_t)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_NewEnvironment(_names: Sexp, values: Sexp, enclosure: Sexp) -> Sexp {
    enter(|| {
        let runtime = runtime();
        let env = runtime.heap.alloc_environment(enclosure)?;
        runtime.heap.root(env);
        let mut cell = values;
        while cell.kind() == SexpType::Pairlist {
            if cell.tag().kind() == SexpType::Symbol {
                runtime.heap.define(env, cell.tag(), cell.car())?;
            }
            cell = cell.cdr();
        }
        Ok(env)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_NewHashedEnv(enclosure: Sexp, _size: c_int) -> Sexp {
    enter(|| Ok(runtime().heap.alloc_environment(enclosure)?))
}

/// The basic classes `methods` treats as built in, which `Rf_isBasicClass` asks about.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_isBasicClass(name: *const std::ffi::c_char) -> crate::Rboolean {
    const BASIC: &[&[u8]] = &[
        b"NULL",
        b"logical",
        b"numeric",
        b"character",
        b"complex",
        b"integer",
        b"double",
        b"raw",
        b"expression",
        b"list",
        b"function",
        b"environment",
        b"name",
        b"call",
        b"language",
        b"externalptr",
        b"S4",
        b"matrix",
        b"array",
        b"ts",
        b"data.frame",
        b"factor",
        b"ordered",
        b"formula",
        b"oldClass",
    ];
    BASIC.contains(&crate::strings::c_bytes(name)) as crate::Rboolean
}

/// `Rf_S3Class(x)`: the `.S3Class` slot of an S4 object that extends an S3 class.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_S3Class(object: Sexp) -> Sexp {
    enter(|| {
        let symbol = runtime().heap.install(b".S3Class")?;
        Ok(crate::attributes::raw_attribute(object, symbol))
    })
}

/// `Rf_type2rstr(type)`: the type's name as a `CHARSXP`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_type2rstr(kind: SEXPTYPE) -> Sexp {
    enter(|| {
        let kind = sexp_type(kind)?;
        crate::strings::mk_char(kind.name().as_bytes(), heap::strings::Encoding::Native)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_strIsASCII(text: *const std::ffi::c_char) -> crate::Rboolean {
    crate::strings::c_bytes(text).is_ascii() as crate::Rboolean
}

/// `utf8Valid(s)`: whether a C string is valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn utf8Valid(text: *const std::ffi::c_char) -> crate::Rboolean {
    std::str::from_utf8(crate::strings::c_bytes(text)).is_ok() as crate::Rboolean
}

/// `Rstrdup(s)`: a `malloc` copy of a C string, which the caller frees.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rstrdup(text: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    libc::strdup(text)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_strchr(
    text: *const std::ffi::c_char,
    character: c_int,
) -> *mut std::ffi::c_char {
    libc::strchr(text, character)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_strrchr(
    text: *const std::ffi::c_char,
    character: c_int,
) -> *mut std::ffi::c_char {
    libc::strrchr(text, character)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_FileExists(path: *const std::ffi::c_char) -> crate::Rboolean {
    let path = String::from_utf8_lossy(crate::strings::c_bytes(path)).into_owned();
    std::path::Path::new(&path).exists() as crate::Rboolean
}

/// `Ri18n_wcwidth(c)`: the terminal width of a code point.
#[unsafe(no_mangle)]
pub extern "C" fn Ri18n_wcwidth(code: u32) -> c_int {
    char::from_u32(code).map_or(1, |character| {
        unicode_width::UnicodeWidthChar::width(character).unwrap_or(0) as c_int
    })
}

/// `R_shallow_duplicate_attr(x)`: a copy of `x` that may share its data but owns its
/// attributes, so they can be changed without affecting `x`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_shallow_duplicate_attr(object: Sexp) -> Sexp {
    enter(|| crate::memory::duplicate(object, false))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_duplicate_attr(object: Sexp) -> Sexp {
    enter(|| crate::memory::duplicate(object, false))
}

/// `Rf_copyMostAttribNoTs(from, to)`: `Rf_copyMostAttrib` without the time-series attribute,
/// and with a `ts` class dropped.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_copyMostAttribNoTs(from: Sexp, to: Sexp) {
    enter(|| {
        use crate::globals::{
            R_ClassSymbol, R_DimNamesSymbol, R_DimSymbol, R_NamesSymbol, R_TspSymbol,
        };
        let runtime = runtime();
        let mut cell = from.attrib();
        while cell.kind() == SexpType::Pairlist {
            let tag = cell.tag();
            if tag == R_ClassSymbol.get() {
                let class = cell.car();
                let kept: Vec<Sexp> = (0..class.length() as usize)
                    .map(|index| class.element(index))
                    .filter(|name| name.bytes() != b"ts")
                    .collect();
                if !kept.is_empty() {
                    let value = runtime
                        .heap
                        .alloc_vector(SexpType::String, kept.len() as isize)?;
                    runtime.heap.root(value);
                    for (index, name) in kept.into_iter().enumerate() {
                        runtime.heap.set_element(value, index, name);
                    }
                    crate::attributes::install_attribute(to, tag, value)?;
                }
            } else if tag != R_NamesSymbol.get()
                && tag != R_DimSymbol.get()
                && tag != R_DimNamesSymbol.get()
                && tag != R_TspSymbol.get()
            {
                crate::attributes::install_attribute(to, tag, cell.car())?;
            }
            cell = cell.cdr();
        }
        to.set_object(
            crate::attributes::raw_attribute(to, R_ClassSymbol.get()).kind() != SexpType::Nil,
        );
        to.set_gp_bits(
            heap::object::gp::S4_OBJECT,
            from.has_gp(heap::object::gp::S4_OBJECT),
        );
        Ok(())
    })
}

/// `R_body_no_src(f)`: a closure's body without its source references.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_body_no_src(closure: Sexp) -> Sexp {
    enter(|| {
        let body = checked(closure, SexpType::Closure, "R_body_no_src")?.cdr();
        let copy = crate::memory::duplicate(body, false)?;
        runtime().heap.root(copy);
        for name in [b"srcref".as_slice(), b"srcfile", b"wholeSrcref"] {
            let symbol = runtime().heap.install(name)?;
            crate::attributes::remove_attribute(copy, symbol);
        }
        Ok(copy)
    })
}

/// `Rf_substitute(lang, rho)`: `lang` with each symbol bound in `rho` replaced by its value, a
/// promise by its expression, and `...` spliced in, as `substitute()` does.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_substitute(expression: Sexp, env: Sexp) -> Sexp {
    enter(|| substitute(expression, env))
}

pub unsafe fn substitute(expression: Sexp, env: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let known = *runtime.heap.known();
    match expression.kind() {
        SexpType::Promise => substitute(expression.cdr(), env),
        SexpType::Symbol => {
            if env == known.global_env || env.kind() != SexpType::Environment {
                return Ok(expression);
            }
            let value = crate::environments::frame_value(env, expression);
            if value == known.unbound {
                return Ok(expression);
            }
            Ok(match value.kind() {
                SexpType::Promise => value.cdr(),
                SexpType::Dots => expression,
                _ if value == known.missing_arg => expression,
                _ => value,
            })
        }
        SexpType::Language => {
            let substituted = substitute_list(expression, env)?;
            runtime.heap.root(substituted);
            if substituted.kind() == SexpType::Pairlist {
                runtime.heap.retype(substituted, SexpType::Language)?;
            }
            Ok(substituted)
        }
        _ => Ok(expression),
    }
}

unsafe fn substitute_list(list: Sexp, env: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let known = *runtime.heap.known();
    let mut items: Vec<(Sexp, Sexp)> = Vec::new();
    let mut cell = list;
    while matches!(cell.kind(), SexpType::Pairlist | SexpType::Language) {
        let element = cell.car();
        if element == crate::globals::R_DotsSymbol.get() && env.kind() == SexpType::Environment {
            let dots = crate::environments::frame_value(env, element);
            if dots.kind() == SexpType::Dots {
                let mut dot = dots;
                while dot.kind() == SexpType::Dots {
                    let value = substitute(dot.car(), known.empty_env)?;
                    runtime.heap.root(value);
                    items.push((dot.tag(), value));
                    dot = dot.cdr();
                }
                cell = cell.cdr();
                continue;
            }
            if dots == known.missing_arg {
                cell = cell.cdr();
                continue;
            }
        }
        let value = substitute(element, env)?;
        runtime.heap.root(value);
        items.push((cell.tag(), value));
        cell = cell.cdr();
    }
    let mut result = known.nil;
    for &(tag, value) in items.iter().rev() {
        result = runtime
            .heap
            .alloc_triple(SexpType::Pairlist, value, result, tag)?;
        runtime.heap.root(result);
    }
    Ok(result)
}
