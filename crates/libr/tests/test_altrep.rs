//! ALTREP through the C API: classes of every kind defined by the `altreptest` package, ry's
//! built-in classes, GNU R conformance, method errors, collection stress, and a property test
//! comparing ALTREP views with ordinary vectors.

mod harness;

use harness::*;
use std::{
    ffi::{CStr, c_int},
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};
use syntax::testing::{SplitMix64, fuzz_iterations};

const CPLXSXP: c_int = 15;
const NA_INTEGER: c_int = i32::MIN;

fn altreptest(r: &R) {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    let library = BUILT.get_or_init(|| build_package("altreptest", Headers::Ry));
    r.load(library);
}

fn expected_report() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/packages/altreptest/expected.txt")
}

/// A libR function the shared harness does not bind.
fn function<T: Copy>(r: &R, name: &str) -> T {
    let symbol = format!("{name}\0");
    unsafe {
        *r.api()
            .library
            .get::<T>(symbol.as_bytes())
            .unwrap_or_else(|error| panic!("libR does not export {name}: {error}"))
    }
}

fn make(r: &R, class: &str, length: c_int) -> Sexp {
    let class = r.string(class);
    r.call("altreptest_make", &[class, r.integer(length)])
        .expect("an ALTREP instance")
}

fn integer_vector(r: &R, values: &[c_int]) -> Sexp {
    let vector = r.keep(unsafe { (r.api().Rf_allocVector)(INTSXP, values.len() as isize) });
    unsafe {
        std::slice::from_raw_parts_mut((r.api().INTEGER)(vector), values.len())
            .copy_from_slice(values)
    };
    vector
}

fn string_vector(r: &R, values: &[&str]) -> Sexp {
    let vector = r.keep(unsafe { (r.api().Rf_allocVector)(STRSXP, values.len() as isize) });
    for (index, value) in values.iter().enumerate() {
        let element = unsafe {
            (r.api().Rf_mkCharLenCE)(value.as_ptr().cast(), value.len() as c_int, 1)
        };
        unsafe { (r.api().SET_STRING_ELT)(vector, index as isize, element) };
    }
    vector
}

fn class_of(r: &R, value: Sexp) -> Option<Vec<Option<String>>> {
    let class = r.call("altreptest_class", &[value]).expect("class info");
    (r.type_of(class) == STRSXP).then(|| r.strings(class))
}

fn identical(r: &R, left: Sexp, right: Sexp) -> bool {
    unsafe { (r.api().R_compute_identical)(left, right, 16) != 0 }
}

#[test]
fn the_report_matches_gnu_r() {
    let r = r();
    altreptest(&r);
    let report = r.call("altreptest_report", &[]).expect("the report");
    let actual: Vec<String> = r.strings(report).into_iter().flatten().collect();
    let expected = std::fs::read_to_string(expected_report()).expect("expected.txt");
    let expected: Vec<&str> = expected.lines().collect();
    for (line, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(actual, expected, "report line {}", line + 1);
    }
    assert_eq!(actual.len(), expected.len(), "report length");
}

/// Regenerates the expectation with GNU R, so the committed file cannot drift from it.
#[test]
fn the_expectation_is_gnu_r_s_report() {
    if !gnu_r_available() {
        return;
    }
    let library = build_package("altreptest", Headers::GnuR);
    let output = temporary_file("altreptest-gnu-r.txt");
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/packages/altreptest/expected.R");
    let status = Command::new("Rscript")
        .arg(script)
        .arg(&library)
        .arg(&output)
        .status()
        .expect("Rscript runs");
    assert!(status.success());
    assert_eq!(
        std::fs::read_to_string(output).expect("GNU R's report"),
        std::fs::read_to_string(expected_report()).expect("expected.txt"),
        "regenerate expected.txt with expected.R"
    );
}

#[test]
fn classes_have_gnu_r_s_layout_and_names() {
    let r = r();
    altreptest(&r);
    let x = make(&r, "elt_only", 4);
    assert_eq!(
        class_of(&r, x),
        Some(vec![
            Some("elt_only".into()),
            Some("altreptest".into()),
            Some("integer".into())
        ])
    );
    let names = r.call("altreptest_class_name", &[x]).expect("class name");
    let printname: unsafe extern "C" fn(Sexp) -> Sexp = function(&r, "PRINTNAME");
    let text = |symbol: Sexp| unsafe {
        CStr::from_ptr((r.api().R_CHAR)(printname(symbol)))
            .to_string_lossy()
            .into_owned()
    };
    assert_eq!(text(r.element(names, 0)), "elt_only");
    assert_eq!(text(r.element(names, 1)), "altreptest");
    let plain = r.integer(1);
    let names = r.call("altreptest_class_name", &[plain]).expect("class name");
    assert_eq!(r.element(names, 0), r.nil());
}

#[test]
fn re_registering_a_class_updates_its_instances() {
    let r = r();
    altreptest(&r);
    let x = make(&r, "elt_only", 3);
    assert_eq!(
        r.integers(r.call("altreptest_integer_elts", &[x]).expect("elts")),
        vec![0, 2, 4]
    );
    let logical: unsafe extern "C" fn(c_int) -> Sexp = function(&r, "Rf_ScalarLogical");
    let yes = r.keep(unsafe { logical(1) });
    r.call("altreptest_reregister", &[yes]).expect("re-register");
    let shifted = r.call("altreptest_integer_elts", &[x]).expect("elts");
    let no = r.keep(unsafe { logical(0) });
    r.call("altreptest_reregister", &[no]).expect("re-register");
    assert_eq!(r.integers(shifted), vec![1000, 1001, 1002]);
}

#[test]
fn defaults_fall_back_as_gnu_r_s_do() {
    let r = r();
    altreptest(&r);
    r.call("altreptest_counters", &[]).expect("reset");
    // Get_region falls back to Elt one element at a time.
    let x = make(&r, "elt_only", 5);
    let region = r
        .call("altreptest_integer_region", &[x, r.integer(1), r.integer(3)])
        .expect("region");
    assert_eq!(r.integers(region), vec![2, 4, 6]);
    assert_eq!(
        r.integers(r.call("altreptest_counters", &[]).expect("counters")),
        vec![3, 0, 0]
    );
    // A region past the end is cut short, and one starting at the end is empty.
    let region = r
        .call("altreptest_integer_region", &[x, r.integer(5), r.integer(3)])
        .expect("region");
    assert_eq!(r.length(region), 0);
    // A class without Sum, Min or Max has no answer.
    let sums = r.call("altreptest_sums", &[x]).expect("sums");
    assert_eq!(r.element(sums, 0), r.nil());
    // A string class without Dataptr still duplicates and coerces through Elt in ry.
    let strings = string_vector(&r, &["7", "x"]);
    let view = r.call("altreptest_view", &[strings]).expect("view");
    let copy = r.call("altreptest_duplicate", &[view]).expect("duplicate");
    assert!(class_of(&r, copy).is_none());
    assert_eq!(r.strings(copy), vec![Some("7".into()), Some("x".into())]);
    let numbers = r
        .call("altreptest_coerce", &[view, r.integer(INTSXP)])
        .expect("coerce");
    assert_eq!(r.integers(numbers), vec![7, NA_INTEGER]);
    assert_eq!(r.warnings(), vec!["NAs introduced by coercion".to_string()]);
    // Duplicating a class with its own Duplicate keeps the class.
    let view = r
        .call("altreptest_view", &[integer_vector(&r, &[1, 2])])
        .expect("view");
    let copy = r.call("altreptest_shallow_duplicate", &[view]).expect("copy");
    assert_eq!(class_of(&r, copy).expect("ALTREP")[0], Some("allocating".into()));
    assert_eq!(
        r.integers(r.call("altreptest_counters", &[]).expect("counters"))[2],
        1
    );
}

#[test]
fn errors_in_methods_surface_as_r_errors() {
    let r = r();
    altreptest(&r);
    let depth = unsafe { (r.api().ry_context_depth)() };
    let x = make(&r, "failing", 6);
    let error = r
        .call("altreptest_integer_elts", &[x])
        .expect_err("Elt raises");
    assert!(error.contains("element 3 is unavailable"), "{error}");
    let error = r
        .call("altreptest_integer_pointer", &[x])
        .expect_err("Dataptr raises");
    assert!(error.contains("no data pointer for a failing vector"), "{error}");
    let error = r.call("altreptest_sums", &[x]).expect_err("Sum raises");
    assert!(error.contains("cannot sum a failing vector"), "{error}");
    let error = r
        .call("altreptest_duplicate", &[x])
        .expect_err("duplicate reads elements");
    assert!(error.contains("element 3 is unavailable"), "{error}");
    let error = r
        .call("altreptest_make", &[r.string("nonsense"), r.integer(1)])
        .expect_err("unknown class");
    assert!(error.contains("unknown class 'nonsense'"), "{error}");
    assert_eq!(unsafe { (r.api().ry_context_depth)() }, depth);
    assert_eq!(r.runtime_protect_depth(), 0);
    // The vector still works after its methods failed.
    let region = r
        .call("altreptest_integer_region", &[x, r.integer(0), r.integer(3)])
        .expect("region before the failing element");
    assert_eq!(r.integers(region), vec![0, 1, 2]);
}

#[test]
fn a_writable_list_pointer_keeps_what_is_stored_through_it() {
    let r = r();
    altreptest(&r);
    let list = r.keep(unsafe { (r.api().Rf_allocVector)(VECSXP, 2) });
    let view = r.call("altreptest_view", &[list]).expect("view");
    // Age the view and its storage, so the stored object is a young child of an old parent.
    unsafe { (r.api().ry_collect)(1) };
    let stored = r
        .call("altreptest_store_through_pointer", &[view])
        .expect("store");
    assert_eq!(r.strings(stored), vec![Some("stored through the pointer".into())]);
    unsafe { (r.api().ry_collect)(0) };
    let again = r.call("altreptest_list_elts", &[view]).expect("elements");
    assert_eq!(
        r.strings(r.element(again, 0)),
        vec![Some("stored through the pointer".into())]
    );
}

#[test]
fn built_in_classes_behave_as_gnu_r_s() {
    let r = r();
    altreptest(&r);
    let range: unsafe extern "C" fn(isize, isize) -> Sexp = function(&r, "ry_compact_intrange");
    let sequence = r.keep(unsafe { range(1, 10) });
    assert_eq!(
        class_of(&r, sequence).expect("compact")[..2],
        [Some("compact_intseq".into()), Some("base".into())]
    );
    let data = r.call("altreptest_data", &[sequence]).expect("data");
    assert_eq!(r.reals(r.element(data, 0)), vec![10.0, 1.0, 1.0]);
    assert_eq!(r.element(data, 1), r.nil());
    let sorted = r.call("altreptest_sorted", &[sequence]).expect("sorted");
    assert_eq!(r.integers(sorted), vec![1, 1]);
    let sums = r.call("altreptest_sums", &[sequence]).expect("sums");
    assert_eq!(r.integers(r.element(sums, 0)), vec![55]);
    let descending = r.keep(unsafe { range(5, 3) });
    assert_eq!(
        r.integers(r.call("altreptest_integer_elts", &[descending]).expect("elts")),
        vec![5, 4, 3]
    );
    assert_eq!(
        r.integers(r.call("altreptest_sorted", &[descending]).expect("sorted")),
        vec![-1, 1]
    );
    let big = r.keep(unsafe { range(1, 100_000) });
    let sums = r.call("altreptest_sums", &[big]).expect("sums");
    assert_eq!(r.reals(r.element(sums, 0)), vec![5_000_050_000.0]);
    // `as.double(1:n)` stays compact; `as.character(1:n)` is a deferred conversion.
    let doubles = r
        .call("altreptest_coerce", &[sequence, r.integer(REALSXP)])
        .expect("as double");
    assert_eq!(class_of(&r, doubles).expect("compact")[0], Some("compact_realseq".into()));
    assert_eq!(r.reals(doubles)[9], 10.0);
    let strings = r
        .call("altreptest_coerce", &[sequence, r.integer(STRSXP)])
        .expect("as character");
    assert_eq!(class_of(&r, strings).expect("deferred")[0], Some("deferred_string".into()));
    assert_eq!(r.strings(strings)[9], Some("10".into()));
    // Writing materializes the sequence into data2 and drops what it vouched for.
    let written = r
        .call("altreptest_set_integer", &[sequence, r.integer(1), r.integer(5)])
        .expect("write");
    assert_eq!(
        r.integers(r.call("altreptest_integer_elts", &[written]).expect("elts"))[..3],
        [1, 5, 3]
    );
    let data = r.call("altreptest_data", &[sequence]).expect("data");
    assert_eq!(r.type_of(r.element(data, 1)), INTSXP);
    assert_eq!(
        r.integers(r.call("altreptest_sorted", &[sequence]).expect("sorted"))[1],
        0
    );
    let copy = r.call("altreptest_duplicate", &[sequence]).expect("copy");
    assert!(class_of(&r, copy).is_none());
    // A one-element range is an ordinary scalar.
    let single = r.keep(unsafe { range(4, 4) });
    assert!(class_of(&r, single).is_none());
    // Wrappers carry metadata and unshare on write.
    let source = integer_vector(&r, &[3, 1]);
    let wrap: unsafe extern "C" fn(Sexp) -> Sexp = function(&r, "R_tryWrap");
    let wrapped = r.keep(unsafe { wrap(source) });
    assert_eq!(class_of(&r, wrapped).expect("wrapper")[0], Some("wrap_integer".into()));
    let complex = r.keep(unsafe { (r.api().Rf_allocVector)(CPLXSXP, 1) });
    let wrapped_complex = r.keep(unsafe { wrap(complex) });
    assert_eq!(
        class_of(&r, wrapped_complex).expect("wrapper")[0],
        Some("wrap_complex".into())
    );
}

#[test]
fn methods_that_allocate_survive_collection_stress() {
    let r = r();
    altreptest(&r);
    r.stress(true);
    let values: Vec<c_int> = (0..40).map(|index| index * 3 - 7).collect();
    let source = integer_vector(&r, &values);
    let view = r.call("altreptest_view", &[source]).expect("view");
    assert_eq!(
        r.integers(r.call("altreptest_integer_elts", &[view]).expect("elts")),
        values
    );
    let copy = r.call("altreptest_duplicate", &[view]).expect("copy");
    assert!(identical(&r, copy, source));
    let doubles = r
        .call("altreptest_coerce", &[view, r.integer(REALSXP)])
        .expect("as double");
    assert_eq!(r.reals(doubles)[1], -3.75);
    let strings = r
        .call("altreptest_coerce", &[source, r.integer(STRSXP)])
        .expect("deferred");
    let expected: Vec<Option<String>> = values.iter().map(|v| Some(v.to_string())).collect();
    assert_eq!(r.strings(strings), expected);
    let texts: Vec<String> = (0..30).map(|index| format!("s{index}")).collect();
    let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let strings = string_vector(&r, &text_refs);
    let view = r.call("altreptest_view", &[strings]).expect("string view");
    let copy = r.call("altreptest_duplicate", &[view]).expect("copy");
    assert!(identical(&r, copy, strings));
    r.stress(false);
}

/// Random operations on an ALTREP view and on an ordinary copy must agree.
#[test]
fn altrep_views_match_ordinary_vectors() {
    let r = r();
    altreptest(&r);
    let wrap: unsafe extern "C" fn(Sexp) -> Sexp = function(&r, "R_tryWrap");
    for seed in 0..fuzz_iterations(40) as u64 {
        let mut rng = SplitMix64(seed);
        let mark = r.protected.get();
        r.stress(seed % 5 == 4);
        let length = rng.below(12) + 1;
        let values: Vec<c_int> = (0..length)
            .map(|_| {
                if rng.chance(1, 8) {
                    NA_INTEGER
                } else {
                    rng.below(200) as c_int - 100
                }
            })
            .collect();
        let plain = integer_vector(&r, &values);
        let mut model = values.clone();
        let view = if rng.chance(1, 2) {
            r.call("altreptest_view", &[plain]).expect("view")
        } else {
            let copy = r.call("altreptest_duplicate", &[plain]).expect("copy");
            r.keep(unsafe { wrap(copy) })
        };
        for _ in 0..8 {
            match rng.below(6) {
                0 => {
                    let elements = r.call("altreptest_integer_elts", &[view]).expect("elts");
                    assert_eq!(r.integers(elements), model, "seed {seed}");
                }
                1 => {
                    let start = rng.below(length + 2);
                    let count = rng.below(length + 2);
                    let region = r
                        .call(
                            "altreptest_integer_region",
                            &[view, r.integer(start as c_int), r.integer(count as c_int)],
                        )
                        .expect("region");
                    let end = (start + count).min(length).max(start.min(length));
                    assert_eq!(
                        r.integers(region),
                        model[start.min(length)..end].to_vec(),
                        "seed {seed}"
                    );
                }
                2 => {
                    let index = rng.below(length);
                    let value = rng.below(1000) as c_int;
                    r.call(
                        "altreptest_set_integer",
                        &[view, r.integer(index as c_int), r.integer(value)],
                    )
                    .expect("write");
                    model[index] = value;
                }
                3 => {
                    let copy = r.call("altreptest_duplicate", &[view]).expect("copy");
                    assert_eq!(r.integers(copy), model, "seed {seed}");
                }
                4 => {
                    let strings = r
                        .call("altreptest_coerce", &[view, r.integer(STRSXP)])
                        .expect("as character");
                    let expected: Vec<Option<String>> = model
                        .iter()
                        .map(|&value| (value != NA_INTEGER).then(|| value.to_string()))
                        .collect();
                    assert_eq!(r.strings(strings), expected, "seed {seed}");
                }
                _ => {
                    let doubles = r
                        .call("altreptest_coerce", &[view, r.integer(REALSXP)])
                        .expect("as double");
                    let reals = r.reals(doubles);
                    for (&value, &real) in model.iter().zip(&reals) {
                        if value != NA_INTEGER {
                            let shift = if class_of(&r, view)
                                .is_some_and(|class| class[0].as_deref() == Some("allocating"))
                            {
                                0.25
                            } else {
                                0.0
                            };
                            assert_eq!(real, value as f64 + shift, "seed {seed}");
                        }
                    }
                }
            }
        }
        r.stress(false);
        r.release_to(mark);
    }
}
