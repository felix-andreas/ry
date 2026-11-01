//! Pairlists, calls, closures and promises: the linked structures native code walks with `CAR`
//! and `CDR`.

use {
    crate::{
        boundary::{ApiResult, Reachable, enter, error},
        globals::R_NamesSymbol,
        runtime,
    },
    heap::object::{Sexp, SexpType, Shape},
    std::ffi::c_int,
};

unsafe fn triple(object: Sexp, accessor: &str) -> ApiResult<Sexp> {
    crate::vectors::refuse_native(object)?;
    if object.kind().shape() != Shape::Triple || object.is_altrep() {
        return Err(error(format!(
            "bad {accessor}: a '{}' has no cells",
            object.kind().name()
        )));
    }
    Ok(object)
}

fn is_cell(kind: SexpType) -> bool {
    matches!(
        kind,
        SexpType::Pairlist | SexpType::Language | SexpType::Dots
    )
}

/// `Rf_PairToVectorList`: a list with the pairlist's elements, named by its tags.
pub unsafe fn pairlist_to_vector(list: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let mut cells = Vec::new();
    let mut cell = list;
    while is_cell(cell.kind()) {
        cells.push(cell);
        cell = cell.cdr();
    }
    let vector = runtime
        .heap
        .alloc_vector(SexpType::List, cells.len() as isize)?;
    runtime.heap.root(vector);
    for (index, cell) in cells.iter().enumerate() {
        runtime.heap.set_element(vector, index, cell.car());
    }
    if cells.iter().any(|cell| cell.tag() != runtime.nil()) {
        let names = runtime
            .heap
            .alloc_vector(SexpType::String, cells.len() as isize)?;
        runtime.heap.root(names);
        for (index, cell) in cells.iter().enumerate() {
            if cell.tag().kind() == SexpType::Symbol {
                runtime.heap.set_element(names, index, cell.tag().car());
            }
        }
        crate::attributes::install_attribute(vector, R_NamesSymbol.get(), names)?;
    }
    copy_other_attributes(list, vector)?;
    Ok(vector)
}

/// `Rf_VectorToPairList`: a pairlist with the list's elements, tagged by its names.
pub unsafe fn vector_to_pairlist(vector: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let nil = runtime.nil();
    let length = crate::vectors::xlength(vector)? as usize;
    let names = crate::attributes::raw_attribute(vector, R_NamesSymbol.get());
    let mut list = nil;
    for index in (0..length).rev() {
        let name = if names.kind() == SexpType::String {
            crate::vectors::sexp_elt(names, index)?
        } else {
            nil
        };
        let tag = if name != nil && !name.bytes().is_empty() {
            runtime.heap.install_char(name)?
        } else {
            nil
        };
        let element = crate::vectors::sexp_elt(vector, index)?;
        list = runtime
            .heap
            .alloc_triple(SexpType::Pairlist, element, list, tag)?;
        runtime.heap.root(list);
    }
    if list != nil {
        copy_other_attributes(vector, list)?;
    }
    Ok(list)
}

/// A call or `...` list copied cell by cell into an ordinary pairlist.
pub unsafe fn copy_as_pairlist(list: Sexp) -> ApiResult<Sexp> {
    let runtime = runtime();
    let mut cells = Vec::new();
    let mut cell = list;
    while is_cell(cell.kind()) {
        cells.push(cell);
        cell = cell.cdr();
    }
    let mut copy = runtime.nil();
    for original in cells.iter().rev() {
        copy =
            runtime
                .heap
                .alloc_triple(SexpType::Pairlist, original.car(), copy, original.tag())?;
        runtime.heap.root(copy);
    }
    Ok(copy)
}

unsafe fn copy_other_attributes(from: Sexp, to: Sexp) -> ApiResult<()> {
    let mut cell = from.attrib();
    while cell.kind() == SexpType::Pairlist {
        if cell.tag() != R_NamesSymbol.get() {
            crate::attributes::install_attribute(to, cell.tag(), cell.car())?;
        }
        cell = cell.cdr();
    }
    to.set_object(from.is_object());
    Ok(())
}

pub unsafe fn list_of(kind: SexpType, elements: &[Sexp]) -> ApiResult<Sexp> {
    let runtime = runtime();
    for &element in elements {
        runtime.heap.root(element);
    }
    let nil = runtime.nil();
    let mut list = nil;
    for (position, &element) in elements.iter().enumerate().rev() {
        let cell_kind = if position == 0 {
            kind
        } else {
            SexpType::Pairlist
        };
        list = runtime.heap.alloc_triple(cell_kind, element, list, nil)?;
        runtime.heap.root(list);
    }
    Ok(list)
}

// Every reader below returns a field of its argument, which stays reachable through it, so the
// results are `Reachable` and take no handle. A walk over list cells, the common case, skips the
// boundary altogether, as `Rinternals.h` does inline.

macro_rules! read_path {
    ($name:ident, $($step:ident),+) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(object: Sexp) -> Sexp {
            let steps: &[unsafe fn(Sexp) -> Sexp] = &[$(Sexp::$step),+];
            let mut value = object;
            for step in steps {
                if !is_cell(value.kind()) {
                    return read_path_slowly(object, steps, stringify!($name));
                }
                value = step(value);
            }
            value
        }
    };
}

/// A path that reaches something other than a list cell, which `triple` accepts or reports.
unsafe fn read_path_slowly(
    object: Sexp,
    steps: &[unsafe fn(Sexp) -> Sexp],
    accessor: &str,
) -> Sexp {
    enter(|| {
        let mut value = object;
        for step in steps {
            value = step(triple(value, accessor)?);
        }
        Ok(Reachable(value))
    })
    .0
}

/// GNU R lays an external pointer out as a cell, with the address in `CAR`, the protected
/// value in `CDR` and the tag in `TAG`, and older native code reads it that way.
macro_rules! read_cell {
    ($name:ident, $step:ident, $external:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(object: Sexp) -> Sexp {
            if is_cell(object.kind()) {
                return object.$step();
            }
            enter(|| {
                if object.kind() == SexpType::ExternalPtr {
                    let read: unsafe fn(Sexp) -> Sexp = $external;
                    return Ok(Reachable(read(object)));
                }
                Ok(Reachable(triple(object, stringify!($name))?.$step()))
            })
            .0
        }
    };
}

// The address is handed back to native code as an opaque word and never dereferenced by ry. A
// null address has no `Sexp` form, so it reads as `R_NilValue`.
read_cell!(CAR, car, |object| unsafe {
    Sexp::from_raw(object.external_address().cast()).unwrap_or_else(|| runtime().nil())
});
read_cell!(CDR, cdr, |object| unsafe { object.external_protected() });

/// GNU R leaves a bytecode object's tag unused. ry keeps the inline caches there, and their
/// environment binding cells are internal objects that native code walking a closure (lobstr's
/// `obj_size`, for one) must not reach.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn TAG(object: Sexp) -> Sexp {
    if is_cell(object.kind()) {
        return object.tag();
    }
    enter(|| match object.kind() {
        SexpType::ExternalPtr => Ok(Reachable(object.external_tag())),
        SexpType::Bytecode => Ok(Reachable(runtime().nil())),
        _ => Ok(Reachable(triple(object, "TAG")?.tag())),
    })
    .0
}
read_path!(CAAR, car, car);
read_path!(CDAR, car, cdr);
read_path!(CADR, cdr, car);
read_path!(CDDR, cdr, cdr);
read_path!(CDDDR, cdr, cdr, cdr);
read_path!(CADDR, cdr, cdr, car);
read_path!(CADDDR, cdr, cdr, cdr, car);
read_path!(CAD4R, cdr, cdr, cdr, cdr, car);
read_path!(CAD5R, cdr, cdr, cdr, cdr, cdr, car);

macro_rules! write_path {
    ($name:ident, $($step:ident),*) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(object: Sexp, value: Sexp) -> Sexp {
            enter(|| {
                #[allow(unused_mut)]
                let mut cell = object;
                $( cell = triple(cell, stringify!($name))?.$step(); )*
                let cell = triple(cell, stringify!($name))?;
                if cell == runtime().nil() {
                    return Err(error(concat!("bad value: ", stringify!($name), " on NULL")));
                }
                runtime().heap.set_car(cell, value);
                Ok(value)
            })
        }
    };
}

write_path!(SETCAR,);
write_path!(SETCADR, cdr);
write_path!(SETCADDR, cdr, cdr);
write_path!(SETCADDDR, cdr, cdr, cdr);
write_path!(SETCAD4R, cdr, cdr, cdr, cdr);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SETCDR(object: Sexp, value: Sexp) -> Sexp {
    enter(|| {
        let cell = triple(object, "SETCDR")?;
        if cell == runtime().nil() {
            return Err(error("bad call to SETCDR"));
        }
        runtime().heap.set_cdr(cell, value);
        Ok(value)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SET_TAG(object: Sexp, value: Sexp) {
    enter(|| {
        let cell = triple(object, "SET_TAG")?;
        if cell == runtime().nil() {
            return Err(error("bad call to SET_TAG"));
        }
        runtime().heap.set_tag(cell, value);
        Ok(())
    })
}

/// The missingness bits of an argument cell.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn MISSING(object: Sexp) -> c_int {
    (object.gp() & 15) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_cons(car: Sexp, cdr: Sexp) -> Sexp {
    enter(|| Ok(runtime().heap.cons(car, cdr)?))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_lcons(car: Sexp, cdr: Sexp) -> Sexp {
    enter(|| {
        let nil = runtime().nil();
        Ok(runtime()
            .heap
            .alloc_triple(SexpType::Language, car, cdr, nil)?)
    })
}

macro_rules! list_constructor {
    ($name:ident, $kind:expr, $($argument:ident),+) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name($($argument: Sexp),+) -> Sexp {
            enter(|| list_of($kind, &[$($argument),+]))
        }
    };
}

list_constructor!(Rf_list1, SexpType::Pairlist, a);
list_constructor!(Rf_list2, SexpType::Pairlist, a, b);
list_constructor!(Rf_list3, SexpType::Pairlist, a, b, c);
list_constructor!(Rf_list4, SexpType::Pairlist, a, b, c, d);
list_constructor!(Rf_list5, SexpType::Pairlist, a, b, c, d, e);
list_constructor!(Rf_list6, SexpType::Pairlist, a, b, c, d, e, f);
list_constructor!(Rf_lang1, SexpType::Language, a);
list_constructor!(Rf_lang2, SexpType::Language, a, b);
list_constructor!(Rf_lang3, SexpType::Language, a, b, c);
list_constructor!(Rf_lang4, SexpType::Language, a, b, c, d);
list_constructor!(Rf_lang5, SexpType::Language, a, b, c, d, e);
list_constructor!(Rf_lang6, SexpType::Language, a, b, c, d, e, f);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_elt(list: Sexp, index: c_int) -> Sexp {
    enter(|| {
        let mut cell = list;
        for _ in 0..index.max(0) {
            cell = triple(cell, "elt")?.cdr();
        }
        Ok(triple(cell, "elt")?.car())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_lastElt(list: Sexp) -> Sexp {
    enter(|| {
        let mut cell = list;
        let mut last = list;
        while is_cell(cell.kind()) {
            last = cell;
            cell = cell.cdr();
        }
        Ok(last)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_nthcdr(list: Sexp, count: c_int) -> Sexp {
    enter(|| {
        let mut cell = list;
        for _ in 0..count.max(0) {
            if !is_cell(cell.kind()) {
                return Err(error("'nthcdr' list shorter than 'n'"));
            }
            cell = cell.cdr();
        }
        Ok(cell)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_listAppend(first: Sexp, second: Sexp) -> Sexp {
    enter(|| {
        let runtime = runtime();
        if first == runtime.nil() {
            return Ok(second);
        }
        let mut cell = first;
        while is_cell(cell.cdr().kind()) {
            cell = cell.cdr();
        }
        runtime.heap.set_cdr(cell, second);
        Ok(first)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_PairToVectorList(list: Sexp) -> Sexp {
    enter(|| pairlist_to_vector(list))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_VectorToPairList(vector: Sexp) -> Sexp {
    enter(|| vector_to_pairlist(vector))
}

unsafe fn checked_kind(object: Sexp, kind: SexpType, accessor: &str) -> ApiResult<Sexp> {
    if object.kind() != kind {
        return Err(error(format!(
            "{accessor}() can only be applied to a '{}', not a '{}'",
            kind.name(),
            object.kind().name()
        )));
    }
    Ok(object)
}

macro_rules! closure_reader {
    ($name:ident, $kind:expr, $slot:ident) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(object: Sexp) -> Sexp {
            enter(|| Ok(checked_kind(object, $kind, stringify!($name))?.$slot()))
        }
    };
}

closure_reader!(FORMALS, SexpType::Closure, car);
closure_reader!(BODY, SexpType::Closure, cdr);
closure_reader!(CLOENV, SexpType::Closure, tag);
closure_reader!(R_ClosureFormals, SexpType::Closure, car);
closure_reader!(R_ClosureBody, SexpType::Closure, cdr);
closure_reader!(R_ClosureEnv, SexpType::Closure, tag);
closure_reader!(PRVALUE, SexpType::Promise, car);
closure_reader!(PRCODE, SexpType::Promise, cdr);
closure_reader!(PRENV, SexpType::Promise, tag);
closure_reader!(R_PromiseExpr, SexpType::Promise, cdr);
closure_reader!(PREXPR, SexpType::Promise, cdr);

macro_rules! promise_writer {
    ($name:ident, $setter:ident) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(object: Sexp, value: Sexp) {
            enter(|| {
                checked_kind(object, SexpType::Promise, stringify!($name))?;
                runtime().heap.$setter(object, value);
                Ok(())
            })
        }
    };
}

promise_writer!(SET_PRVALUE, set_car);
promise_writer!(SET_PRCODE, set_cdr);
promise_writer!(SET_PRENV, set_tag);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_mkClosure(formals: Sexp, body: Sexp, env: Sexp) -> Sexp {
    enter(|| {
        if env.kind() != SexpType::Environment {
            return Err(error("invalid environment"));
        }
        Ok(runtime()
            .heap
            .alloc_triple(SexpType::Closure, formals, body, env)?)
    })
}

/// The expression a compiled body stands for: GNU R keeps it as the first constant of the
/// bytecode's constant pool (code in the first slot, constants in the second).
pub(crate) unsafe fn bytecode_expression(body: Sexp) -> Sexp {
    if body.kind() != SexpType::Bytecode {
        return body;
    }
    let constants = body.cdr();
    if constants.kind() == SexpType::List && constants.length() > 0 {
        constants.element(0)
    } else {
        body
    }
}

/// `R_ClosureExpr(f)`: the body of a closure as an expression, even when it is compiled.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ClosureExpr(closure: Sexp) -> Sexp {
    enter(|| {
        Ok(bytecode_expression(
            checked_kind(closure, SexpType::Closure, "R_ClosureExpr")?.cdr(),
        ))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_BytecodeExpr(body: Sexp) -> Sexp {
    bytecode_expression(body)
}
