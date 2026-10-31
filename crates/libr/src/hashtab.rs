//! R's hash tables (`utils::hashtab`), keyed by `identical()` or by address.
//!
//! A table is an external pointer whose tag holds `c(type, count)` as an integer vector and
//! whose protected value is a list of buckets, each a pairlist of cells with the key in the tag
//! and the value in the car. Keeping every key and value in heap objects means the collector
//! traces the table like any other object and no memory lives outside the heap. The R-level
//! object is a one-element list of class `hashtab` holding the pointer.

use crate::{
    boundary::{ApiResult, Handout, enter, error},
    globals::R_ClassSymbol,
    runtime,
    vectors::{identical, xlength},
};
use heap::object::{Sexp, SexpType, is_na_real};
use std::{
    ffi::{c_int, c_void},
    hash::{Hash, Hasher},
};

/// `R_hashtab_type`, which C passes by value: the table's external pointer.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Hashtab {
    cell: Sexp,
}

impl Handout for Hashtab {
    fn hand_out(self, runtime: &mut crate::Runtime) {
        self.cell.hand_out(runtime);
    }
}

const IDENTICAL: c_int = 0;
const ADDRESS: c_int = 1;
const INITIAL_BUCKETS: usize = 32;

unsafe fn table(table: Hashtab) -> ApiResult<Sexp> {
    let cell = table.cell;
    if cell.kind() != SexpType::ExternalPtr
        || cell.external_tag().kind() != SexpType::Integer
        || cell.external_protected().kind() != SexpType::List
    {
        return Err(error("not a proper hash table object"));
    }
    Ok(cell)
}

unsafe fn header(cell: Sexp) -> &'static mut [i32] {
    std::slice::from_raw_parts_mut(cell.external_tag().data() as *mut i32, 2)
}

unsafe fn hash_key(kind: c_int, key: Sexp) -> ApiResult<u64> {
    let mut hasher = rustc_hash::FxHasher::default();
    if kind == ADDRESS {
        key.hash(&mut hasher);
        return Ok(hasher.finish());
    }
    hash_value(key, &mut hasher, 0)?;
    Ok(hasher.finish())
}

/// Hashes what `identical()` compares, to a bounded depth: equal values hash equally, and the
/// bucket's `identical()` check decides.
unsafe fn hash_value(
    value: Sexp,
    hasher: &mut rustc_hash::FxHasher,
    depth: usize,
) -> ApiResult<()> {
    (value.kind() as u8).hash(hasher);
    if depth > 2 {
        return Ok(());
    }
    match value.kind() {
        SexpType::Symbol | SexpType::Environment => value.hash(hasher),
        // `identical()` compares the address an external pointer holds, not the object.
        SexpType::ExternalPtr => (value.external_address() as usize).hash(hasher),
        // Strings compare by their text, whatever encoding the bytes are in.
        SexpType::Char => crate::strings::utf8_text(value)?.hash(hasher),
        SexpType::Logical | SexpType::Integer => {
            for index in 0..xlength(value)?.min(64) as usize {
                crate::vectors::int_elt(value, index)?.hash(hasher);
            }
        }
        SexpType::Real => {
            for index in 0..xlength(value)?.min(64) as usize {
                let number = crate::vectors::real_elt(value, index)?;
                let bits = if is_na_real(number) {
                    1
                } else if number.is_nan() {
                    2
                } else if number == 0.0 {
                    0
                } else {
                    number.to_bits()
                };
                bits.hash(hasher);
            }
        }
        SexpType::String => {
            for index in 0..xlength(value)?.min(64) as usize {
                crate::strings::utf8_text(crate::vectors::sexp_elt(value, index)?)?.hash(hasher);
            }
        }
        SexpType::List | SexpType::Expression => {
            let length = xlength(value)?;
            length.hash(hasher);
            for index in 0..length.min(8) as usize {
                hash_value(crate::vectors::sexp_elt(value, index)?, hasher, depth + 1)?;
            }
        }
        SexpType::Pairlist | SexpType::Language => {
            hash_value(value.car(), hasher, depth + 1)?;
        }
        _ => {}
    }
    Ok(())
}

unsafe fn same_key(kind: c_int, left: Sexp, right: Sexp) -> ApiResult<bool> {
    if kind == ADDRESS {
        return Ok(left == right);
    }
    identical(left, right, 16)
}

unsafe fn bucket_index(cell: Sexp, key: Sexp) -> ApiResult<usize> {
    let kind = header(cell)[0];
    let buckets = cell.external_protected();
    Ok((hash_key(kind, key)? % buckets.length() as u64) as usize)
}

/// The cell holding `key`, if any.
unsafe fn find(cell: Sexp, key: Sexp) -> ApiResult<Option<Sexp>> {
    let kind = header(cell)[0];
    let index = bucket_index(cell, key)?;
    let mut chain = cell.external_protected().element(index);
    while chain.kind() == SexpType::Pairlist {
        if same_key(kind, chain.tag(), key)? {
            return Ok(Some(chain));
        }
        chain = chain.cdr();
    }
    Ok(None)
}

pub unsafe fn make(kind: c_int, size: c_int) -> ApiResult<Hashtab> {
    if kind != IDENTICAL && kind != ADDRESS {
        return Err(error("bad hash table type"));
    }
    let runtime = runtime();
    let header = runtime.heap.alloc_vector(SexpType::Integer, 2)?;
    runtime.heap.root(header);
    *(header.data() as *mut i32) = kind;
    let count = (size.max(0) as usize)
        .next_power_of_two()
        .max(INITIAL_BUCKETS);
    let buckets = runtime.heap.alloc_vector(SexpType::List, count as isize)?;
    runtime.heap.root(buckets);
    let cell = runtime
        .heap
        .alloc_external_ptr(std::ptr::null_mut::<c_void>(), header, buckets)?;
    Ok(Hashtab { cell })
}

unsafe fn grow(cell: Sexp) -> ApiResult<()> {
    let runtime = runtime();
    let old = cell.external_protected();
    let count = old.length() as usize * 2;
    let buckets = runtime.heap.alloc_vector(SexpType::List, count as isize)?;
    runtime.heap.root(buckets);
    let kind = header(cell)[0];
    for index in 0..old.length() as usize {
        let mut chain = old.element(index);
        while chain.kind() == SexpType::Pairlist {
            let next = chain.cdr();
            let target = (hash_key(kind, chain.tag())? % count as u64) as usize;
            runtime.heap.set_cdr(chain, buckets.element(target));
            runtime.heap.set_element(buckets, target, chain);
            chain = next;
        }
    }
    runtime.heap.set_external_protected(cell, buckets);
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_mkhashtab(kind: c_int, size: c_int) -> Hashtab {
    enter(|| make(kind, size))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_gethash(table_value: Hashtab, key: Sexp, no_match: Sexp) -> Sexp {
    enter(|| {
        let cell = table(table_value)?;
        Ok(find(cell, key)?.map_or(no_match, |found| found.car()))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_sethash(table_value: Hashtab, key: Sexp, value: Sexp) -> Sexp {
    enter(|| {
        let cell = table(table_value)?;
        let runtime = runtime();
        if let Some(found) = find(cell, key)? {
            runtime.heap.set_car(found, value);
            return Ok(value);
        }
        if header(cell)[1] as usize >= cell.external_protected().length() as usize {
            grow(cell)?;
        }
        let index = bucket_index(cell, key)?;
        let buckets = cell.external_protected();
        let entry =
            runtime
                .heap
                .alloc_triple(SexpType::Pairlist, value, buckets.element(index), key)?;
        runtime.heap.set_element(buckets, index, entry);
        header(cell)[1] += 1;
        Ok(value)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_remhash(table_value: Hashtab, key: Sexp) -> c_int {
    enter(|| {
        let cell = table(table_value)?;
        let kind = header(cell)[0];
        let index = bucket_index(cell, key)?;
        let buckets = cell.external_protected();
        let runtime = runtime();
        let mut previous: Option<Sexp> = None;
        let mut chain = buckets.element(index);
        while chain.kind() == SexpType::Pairlist {
            if same_key(kind, chain.tag(), key)? {
                match previous {
                    Some(previous) => runtime.heap.set_cdr(previous, chain.cdr()),
                    None => runtime.heap.set_element(buckets, index, chain.cdr()),
                }
                header(cell)[1] -= 1;
                return Ok(1);
            }
            previous = Some(chain);
            chain = chain.cdr();
        }
        Ok(0)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_numhash(table_value: Hashtab) -> c_int {
    enter(|| Ok(header(table(table_value)?)[1]))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_typhash(table_value: Hashtab) -> c_int {
    enter(|| Ok(header(table(table_value)?)[0]))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_clrhash(table_value: Hashtab) {
    enter(|| {
        let cell = table(table_value)?;
        let buckets = cell.external_protected();
        let runtime = runtime();
        let nil = runtime.nil();
        for index in 0..buckets.length() as usize {
            runtime.heap.set_element(buckets, index, nil);
        }
        header(cell)[1] = 0;
        Ok(())
    })
}

/// The entries as (key, value) pairs, collected first so a callback that changes the table
/// does not disturb the walk. Every pair is rooted here, before any callback runs, because a
/// callback may remove later entries from the table and then allocate.
unsafe fn entries(cell: Sexp) -> Vec<(Sexp, Sexp)> {
    let buckets = cell.external_protected();
    let mut entries = Vec::new();
    for index in 0..buckets.length() as usize {
        let mut chain = buckets.element(index);
        while chain.kind() == SexpType::Pairlist {
            runtime().heap.root(chain.tag());
            runtime().heap.root(chain.car());
            entries.push((chain.tag(), chain.car()));
            chain = chain.cdr();
        }
    }
    entries
}

/// `R_maphash(h, FUN)`: calls the R function `FUN(key, value)` for each entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_maphash(table_value: Hashtab, function: Sexp) -> Sexp {
    enter(|| {
        let cell = table(table_value)?;
        let global = runtime().heap.known().global_env;
        for (key, value) in entries(cell) {
            crate::eval::apply_closure_or_primitive(function, &[key, value], global)?;
        }
        Ok(runtime().nil())
    })
}

unsafe extern "C" {
    fn ry_maphash_call(
        function: unsafe extern "C" fn(Sexp, Sexp, *mut c_void) -> Sexp,
        key: Sexp,
        value: Sexp,
        data: *mut c_void,
        status: *mut c_int,
    ) -> Sexp;
}

/// `R_maphashC(h, FUN, data)`: calls the C function `FUN(key, value, data)` for each entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_maphashC(
    table_value: Hashtab,
    function: unsafe extern "C" fn(Sexp, Sexp, *mut c_void) -> Sexp,
    data: *mut c_void,
) -> Sexp {
    enter(|| {
        let cell = table(table_value)?;
        for (key, value) in entries(cell) {
            let mut status = 0;
            crate::boundary::with_native_scope(|| {
                ry_maphash_call(function, key, value, data, &mut status);
                status
            })?;
        }
        Ok(runtime().nil())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_isHashtable(object: Sexp) -> c_int {
    enter(|| Ok(as_table(object).is_ok() as c_int))
}

unsafe fn as_table(object: Sexp) -> ApiResult<Hashtab> {
    if object.kind() != SexpType::List
        || xlength(object)? != 1
        || !crate::attributes::inherits(object, "hashtab")?
    {
        return Err(error("not a proper hash table object"));
    }
    let table_value = Hashtab {
        cell: object.element(0),
    };
    table(table_value)?;
    Ok(table_value)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_asHashtable(object: Sexp) -> Hashtab {
    enter(|| as_table(object))
}

/// `R_HashtabSEXP(h)`: the R object for a table: a one-element list of class `hashtab`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_HashtabSEXP(table_value: Hashtab) -> Sexp {
    enter(|| {
        let cell = table(table_value)?;
        let runtime = runtime();
        let object = runtime.heap.alloc_vector(SexpType::List, 1)?;
        runtime.heap.root(object);
        runtime.heap.set_element(object, 0, cell);
        let class = crate::strings::mk_char(b"hashtab", heap::strings::Encoding::Native)?;
        let class = crate::memory::scalar_string(class)?;
        crate::attributes::install_attribute(object, R_ClassSymbol.get(), class)?;
        object.set_object(true);
        Ok(object)
    })
}
