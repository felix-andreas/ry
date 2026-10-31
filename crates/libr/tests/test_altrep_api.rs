//! The Rust side of ALTREP that the interpreter and the serializer use: the built-in classes'
//! constructors, `sum`, `inspect`, serialization state, unserialization through the class
//! registry, and `Extract_subset`. The runtime is linked in-process here, so native-style
//! methods are Rust `extern "C"` functions.

// Every helper here works on runtime handles, as the crate's own code does.
#![allow(unsafe_op_in_unsafe_fn)]

use R::{
    altrep::{self, AltrepClass, InspectSubtree},
    boundary::ApiResult,
    runtime, vectors,
};
use heap::{
    GcStress,
    object::{Header, Sexp, SexpType},
};
use std::{
    ffi::{c_int, c_void},
    sync::{Mutex, MutexGuard},
};

static LOCK: Mutex<()> = Mutex::new(());
static CONSOLE: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// The runtime is one per process and single-threaded, so each test holds the lock, and
/// console output is captured.
fn session() -> MutexGuard<'static, ()> {
    let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    runtime().console = Box::new(|text, _| {
        CONSOLE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend_from_slice(text)
    });
    take_console();
    guard
}

fn take_console() -> String {
    let mut console = CONSOLE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    String::from_utf8_lossy(&std::mem::take(&mut *console)).into_owned()
}

fn keep(object: Sexp) -> Sexp {
    runtime().heap.preserve(object);
    object
}

fn ok<T>(result: ApiResult<T>) -> T {
    result.unwrap_or_else(|unwind| panic!("unexpected R error: {unwind:?}"))
}

fn symbol(name: &str) -> Sexp {
    ok(unsafe { runtime().heap.install(name.as_bytes()) }.map_err(Into::into))
}

fn ints(object: Sexp) -> Vec<i32> {
    unsafe {
        let length = ok(vectors::xlength(object));
        (0..length as usize)
            .map(|index| ok(vectors::int_elt(object, index)))
            .collect()
    }
}

fn reals(object: Sexp) -> Vec<f64> {
    unsafe {
        let length = ok(vectors::xlength(object));
        (0..length as usize)
            .map(|index| ok(vectors::real_elt(object, index)))
            .collect()
    }
}

fn strings(object: Sexp) -> Vec<String> {
    unsafe {
        let length = ok(vectors::xlength(object));
        (0..length as usize)
            .map(|index| {
                let element = ok(vectors::sexp_elt(object, index));
                String::from_utf8_lossy(element.bytes()).into_owned()
            })
            .collect()
    }
}

/// Serializes and unserializes `object` through its class, as the serializer will.
unsafe fn round_trip(object: Sexp) -> Sexp {
    let info = altrep::serialized_class(object).expect("an ALTREP object");
    let state = keep(ok(altrep::serialized_state(object)).expect("a serialized state"));
    ok(altrep::unserialize(info, state, object.attrib(), 0, 0)).expect("a registered class")
}

unsafe extern "C" fn print_subtree(object: Sexp, _pre: c_int, _deep: c_int, _pvec: c_int) {
    let text = format!(
        "  <{} of length {}>\n",
        object.kind().name(),
        object.length()
    );
    runtime().console.as_mut()(text.as_bytes(), false);
}

const SUBTREE: InspectSubtree = print_subtree;

#[test]
fn compact_sequences() {
    let _session = session();
    check_compact_sequences();
}

fn check_compact_sequences() {
    unsafe {
        let sequence = keep(ok(altrep::compact_intrange(3, 7)));
        assert_eq!(ints(sequence), vec![3, 4, 5, 6, 7]);
        assert_eq!(ok(altrep::is_sorted(sequence)), altrep::SORTED_INCR);
        assert_eq!(ok(altrep::no_na(sequence)), 1);
        let sum = ok(altrep::sum(sequence, false)).expect("a compact sum");
        assert_eq!(ints(sum), vec![25]);
        assert!(ok(altrep::min(sequence, false)).is_none());
        assert!(ok(altrep::dataptr_or_null(sequence)).is_null());
        let mut buffer = [0; 4];
        let copied = ok(altrep::get_region(sequence, 3, 4, buffer.as_mut_ptr()));
        assert_eq!((copied, &buffer[..2]), (2, &[6, 7][..]));

        assert!(ok(altrep::inspect(sequence, 0, 0, 0, SUBTREE)));
        assert_eq!(take_console(), " 3 : 7 (compact)\n");

        let info = altrep::serialized_class(sequence).expect("a class");
        assert_eq!(info.car(), symbol("compact_intseq"));
        assert_eq!(info.cdr().car(), symbol("base"));
        let copy = round_trip(sequence);
        assert!(copy.is_altrep());
        assert_eq!(ints(copy), vec![3, 4, 5, 6, 7]);

        let descending = keep(ok(altrep::compact_intrange(2, -2)));
        assert_eq!(ints(descending), vec![2, 1, 0, -1, -2]);
        assert_eq!(ok(altrep::is_sorted(descending)), altrep::SORTED_DECR);

        // Expansion keeps the values, and the object then serializes as an ordinary vector.
        ok(altrep::dataptr(sequence, true));
        assert!(!ok(altrep::dataptr_or_null(sequence)).is_null());
        assert!(ok(altrep::serialized_state(sequence)).is_none());
        assert!(ok(altrep::sum(sequence, false)).is_none());
        assert!(ok(altrep::inspect(sequence, 0, 0, 0, SUBTREE)));
        assert_eq!(take_console(), " 3 : 7 (expanded)\n");

        let doubles = keep(ok(altrep::compact_realseq(4, 1.0, 1.0)));
        assert_eq!(reals(doubles), vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(
            reals(ok(altrep::sum(doubles, false)).expect("a sum")),
            vec![10.0]
        );
        assert_eq!(reals(round_trip(doubles)), vec![1.0, 2.0, 3.0, 4.0]);
        assert!(ok(altrep::inspect(doubles, 0, 0, 0, SUBTREE)));
        assert_eq!(take_console(), " 1 : 4 (compact)\n");

        // A range beyond the integer range is a double sequence.
        let wide = keep(ok(altrep::compact_intrange(
            i32::MAX as isize,
            i32::MAX as isize + 2,
        )));
        assert_eq!(wide.kind(), SexpType::Real);
        assert!(altrep::compact_intseq(3, 0, 2).is_err());
        assert!(!ok(altrep::compact_intseq(1, 9, 1)).is_altrep());
    }
}

#[test]
fn deferred_strings() {
    let _session = session();
    check_deferred_strings();
}

fn check_deferred_strings() {
    unsafe {
        let source = keep(ok(R::memory::alloc_vector(SexpType::Real, 3)));
        source
            .slice_mut::<f64>()
            .copy_from_slice(&[0.5, 1e5, heap::object::NA_REAL]);
        let deferred = keep(ok(altrep::deferred_string(source)));
        assert_eq!(source.refcount(), heap::object::REFCOUNT_MAX);
        assert!(ok(altrep::inspect(deferred, 0, 0, 0, SUBTREE)));
        assert_eq!(
            take_console(),
            "  <deferred string conversion>\n  <double of length 3>\n"
        );
        let copy = round_trip(deferred);
        assert_eq!(strings(copy)[..2], ["0.5".to_string(), "1e+05".to_string()]);
        assert_eq!(
            ok(vectors::sexp_elt(copy, 2)),
            runtime().heap.known().na_string
        );
        ok(altrep::dataptr(deferred, false));
        assert!(ok(altrep::serialized_state(deferred)).is_none());
        assert!(ok(altrep::inspect(deferred, 0, 0, 0, SUBTREE)));
        assert_eq!(
            take_console(),
            "  <expanded string conversion>\n  <character of length 3>\n"
        );
        let attributed = keep(ok(R::memory::alloc_vector(SexpType::Integer, 1)));
        ok(R::attributes::set_attrib(attributed, symbol("names"), {
            let name = ok(runtime()
                .heap
                .mk_char(b"a", heap::strings::Encoding::Native)
                .map_err(Into::into));
            keep(ok(R::memory::scalar_string(name)))
        }));
        assert!(altrep::deferred_string(attributed).is_err());
    }
}

#[test]
fn wrappers() {
    let _session = session();
    check_wrappers();
}

fn check_wrappers() {
    unsafe {
        let source = keep(ok(R::memory::alloc_vector(SexpType::Integer, 3)));
        source.slice_mut::<i32>().copy_from_slice(&[1, 2, 4]);
        let wrapped = keep(ok(altrep::wrap(source, altrep::SORTED_INCR, 1)));
        assert_eq!(ok(altrep::is_sorted(wrapped)), altrep::SORTED_INCR);
        assert_eq!(ok(altrep::no_na(wrapped)), 1);
        assert!(ok(altrep::inspect(wrapped, 0, 0, 0, SUBTREE)));
        assert_eq!(
            take_console(),
            " wrapper [srt=1,no_na=1]\n  <integer of length 3>\n"
        );
        let copy = round_trip(wrapped);
        assert_eq!(ints(copy), vec![1, 2, 4]);
        assert_eq!(ok(altrep::is_sorted(copy)), altrep::SORTED_INCR);
        let duplicate = keep(ok(altrep::duplicate(wrapped, true)).expect("a wrapper copy"));
        assert!(duplicate.is_altrep());
        assert_eq!(ok(altrep::no_na(duplicate)), 1);
        // A write forgets the metadata.
        *(ok(altrep::dataptr(wrapped, true)) as *mut i32) = 9;
        assert_eq!(ok(altrep::is_sorted(wrapped)), altrep::UNKNOWN_SORTEDNESS);
        assert_eq!(ints(wrapped), vec![9, 2, 4]);
        let list = keep(ok(R::memory::alloc_vector(SexpType::List, 1)));
        assert_eq!(ok(altrep::wrap(list, 1, 1)), list);
    }
}

#[test]
fn built_in_classes_survive_collection_stress() {
    let _session = session();
    runtime().heap.policy_mut().stress = GcStress::EveryAllocation;
    check_compact_sequences();
    check_deferred_strings();
    check_wrappers();
    runtime().heap.policy_mut().stress = GcStress::Off;
}

// A class whose methods are native-style functions, for the methods no accessor reaches.

unsafe extern "C" fn tens_length(_object: Sexp) -> isize {
    3
}

unsafe extern "C" fn tens_elt(_object: Sexp, index: isize) -> c_int {
    10 * index as c_int
}

unsafe extern "C" fn tens_subset(_object: Sexp, indices: Sexp, _call: Sexp) -> *mut Header {
    indices.as_ptr()
}

unsafe extern "C" fn tens_inspect(
    object: Sexp,
    pre: c_int,
    deep: c_int,
    pvec: c_int,
    subtree: InspectSubtree,
) -> c_int {
    subtree(object.car(), pre, deep, pvec);
    1
}

unsafe extern "C" fn tens_state(object: Sexp) -> *mut Header {
    object.car().as_ptr()
}

unsafe extern "C" fn tens_unserialize(class: Sexp, state: Sexp) -> *mut Header {
    let info = class.attrib();
    let class = ok(altrep::find_class(info.car(), info.cdr().car())).expect("registered");
    let nil = runtime().nil();
    ok(altrep::new_altrep(class, state, nil)).as_ptr()
}

unsafe extern "C" fn tens_max(_object: Sexp, _remove_na: c_int) -> *mut Header {
    ok(R::memory::scalar(SexpType::Integer, 20)).as_ptr()
}

unsafe extern "C" fn tens_duplicate(_object: Sexp, _deep: c_int) -> *mut Header {
    std::ptr::null_mut()
}

unsafe fn tens_class() -> AltrepClass {
    let class = altrep::R_make_altinteger_class(
        c"tens".as_ptr(),
        c"altrepapi".as_ptr(),
        std::ptr::null_mut::<c_void>(),
    );
    altrep::R_set_altrep_Length_method(class, Some(tens_length as _));
    altrep::R_set_altinteger_Elt_method(class, Some(tens_elt as _));
    altrep::R_set_altvec_Extract_subset_method(class, Some(tens_subset as _));
    altrep::R_set_altrep_Inspect_method(class, Some(tens_inspect as _));
    altrep::R_set_altrep_Serialized_state_method(class, Some(tens_state as _));
    altrep::R_set_altrep_Unserialize_method(class, Some(tens_unserialize as _));
    altrep::R_set_altinteger_Max_method(class, Some(tens_max as _));
    altrep::R_set_altrep_Duplicate_method(class, Some(tens_duplicate as _));
    class
}

#[test]
fn native_methods_the_interpreter_and_serializer_call() {
    let _session = session();
    unsafe {
        let class = tens_class();
        let state = keep(ok(R::memory::alloc_vector(SexpType::Raw, 2)));
        let nil = runtime().nil();
        let tens = keep(ok(altrep::new_altrep(class, state, nil)));
        assert_eq!(ints(tens), vec![0, 10, 20]);

        let indices = keep(ok(R::memory::alloc_vector(SexpType::Integer, 1)));
        assert_eq!(
            ok(altrep::extract_subset(tens, indices, nil)),
            Some(indices)
        );
        let plain = keep(ok(R::memory::alloc_vector(SexpType::Integer, 1)));
        assert!(!plain.is_altrep());

        assert!(ok(altrep::inspect(tens, 0, 0, 0, SUBTREE)));
        assert_eq!(take_console(), "  <raw of length 2>\n");

        assert_eq!(ints(ok(altrep::max(tens, true)).expect("a max")), vec![20]);
        assert!(ok(altrep::sum(tens, true)).is_none());
        assert!(ok(altrep::duplicate(tens, false)).is_none());
        let copy = keep(ok(R::memory::duplicate(tens, false)));
        assert!(!copy.is_altrep());
        assert_eq!(ints(copy), vec![0, 10, 20]);

        let attributes = keep(ok(runtime().heap.cons(nil, nil).map_err(Into::into)));
        let info = altrep::serialized_class(tens).expect("a class");
        let restored = ok(altrep::unserialize(info, state, attributes, 1, 0)).expect("restored");
        assert_eq!(ints(restored), vec![0, 10, 20]);
        assert_eq!(restored.attrib(), attributes);
        assert!(restored.is_object());

        // An unknown class is the caller's to handle, and a type mismatch is an error.
        let missing = keep(ok(R::lists::list_of(
            SexpType::Pairlist,
            &[symbol("nothing"), symbol("nowhere"), info.cdr().cdr().car()],
        )));
        assert!(ok(altrep::unserialize(missing, state, nil, 0, 0)).is_none());
        let real_type = keep(ok(R::memory::scalar(SexpType::Integer, 14)));
        let mismatched = keep(ok(R::lists::list_of(
            SexpType::Pairlist,
            &[symbol("tens"), symbol("altrepapi"), real_type],
        )));
        let error = altrep::unserialize(mismatched, state, nil, 0, 0).expect_err("a type mismatch");
        assert!(
            format!("{error:?}").contains("has type double; registered class has type integer")
        );

        assert!(ok(altrep::find_class(symbol("compact_intseq"), symbol("base"))).is_some());
        assert!(ok(altrep::find_class(symbol("tens"), symbol("base"))).is_none());
        assert_eq!(
            ok(altrep::find_class(symbol("tens"), symbol("altrepapi"))),
            Some(class)
        );
    }
}
