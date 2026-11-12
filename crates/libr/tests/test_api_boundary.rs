//! The hot API functions' direct success paths (`boundary::direct`). A differential fuzz runs
//! each operation on its direct path and again with every function forced down its slow path,
//! under collection pressure, and requires the same result, the same error and the same
//! attributes in the same order. An instruction-count test pins what one call of each costs.

#![cfg(unix)]

mod harness;

use {
    harness::*,
    std::{ffi::c_int, process::Command},
    syntax::testing::{SplitMix64, for_each_seed},
};

const CPLXSXP: c_int = 15;
const LISTSXP: c_int = 2;
const LANGSXP: c_int = 6;
const CHARSXP: c_int = 9;

/// One operation of `rytest_api_op`: its code and its three arguments.
struct Operation {
    code: c_int,
    arguments: [Sexp; 3],
}

#[test]
fn fuzz_direct_paths_match_slow_paths() {
    let r = r_with_base();
    rytest(&r);
    for_each_seed(400, |seed| {
        let mut rng = SplitMix64(0xd12ec7 ^ seed);
        let mark = r.protected.get();
        let operation = random_operation(&r, &mut rng);
        let stress = seed % 4 == 3;
        let outcomes = [false, true].map(|slow| {
            let [a, b, c] = operation.arguments;
            // `.Call` evaluates its arguments, so quoting passes symbols and calls as themselves.
            let quoted = [duplicate(&r, a), b, duplicate(&r, c)].map(|argument| r.quote(argument));
            let arguments = [r.integer(operation.code), quoted[0], quoted[1], quoted[2]];
            unsafe { (r.api().ry_set_slow_paths_only)(slow as c_int) };
            r.stress(stress);
            let outcome = r.call("rytest_api_op", &arguments);
            r.stress(false);
            unsafe { (r.api().ry_set_slow_paths_only)(0) };
            outcome
        });
        match &outcomes {
            [Ok(direct), Ok(slow)] => {
                assert_eq!(
                    unsafe { (r.api().R_compute_identical)(*direct, *slow, 0) },
                    1,
                    "seed {seed}: operation {} differs between the direct and the slow path",
                    operation.code
                );
                if operation.code == 27 {
                    let result = unsafe { (r.api().VECTOR_ELT)(*direct, 0) };
                    let separate = unsafe { (r.api().VECTOR_ELT)(result, 1) };
                    assert_eq!(
                        r.integers(separate),
                        [1],
                        "seed {seed}: a copy shares storage with its original"
                    );
                }
                if operation.code == 26 {
                    let result = unsafe { (r.api().VECTOR_ELT)(*direct, 0) };
                    let empty = unsafe { (r.api().VECTOR_ELT)(result, 1) };
                    assert_eq!(
                        r.integers(empty),
                        [1],
                        "seed {seed}: growing a resizable vector exposed a stale element"
                    );
                }
            }
            [Err(direct), Err(slow)] => assert_eq!(direct, slow, "seed {seed}"),
            [direct, slow] => panic!(
                "seed {seed}: operation {}: the direct path gave {direct:?}, the slow path {slow:?}",
                operation.code
            ),
        }
        r.release_to(mark);
    });
}

/// A copy made on the slow path, so a direct path bug cannot reach both runs through their input.
fn duplicate(r: &R, value: Sexp) -> Sexp {
    unsafe {
        (r.api().ry_set_slow_paths_only)(1);
        let copy = (r.api().Rf_duplicate)(value);
        (r.api().ry_set_slow_paths_only)(0);
        r.keep(copy)
    }
}

fn random_operation(r: &R, rng: &mut SplitMix64) -> Operation {
    let code = rng.below(28) as c_int;
    let arguments = match code {
        0 => {
            let object = random_object(r, rng);
            let value = random_value(r, rng, object);
            [object, random_name(r, rng), value]
        }
        1 => [random_object(r, rng), random_name(r, rng), r.nil()],
        25 => {
            let object = random_object(r, rng);
            let value = random_value(r, rng, object);
            [object, r.integer(rng.below(4) as c_int), value]
        }
        7 => {
            let class = ["first", "factor", "data.frame", "rytest_class"][rng.below(4)];
            [random_object(r, rng), r.string(class), r.nil()]
        }
        12..=14 => [
            strings(r, &[["", "a", "rytest.tag", "dim"][rng.below(4)]]),
            r.nil(),
            r.nil(),
        ],
        15 => {
            let kinds = [
                LGLSXP, INTSXP, REALSXP, CPLXSXP, STRSXP, VECSXP, RAWSXP, LISTSXP, LANGSXP,
                CHARSXP, 99,
            ];
            let kind = kinds[rng.below(kinds.len())];
            let length = [-1, 0, 1, 5][rng.below(4)];
            [r.integer(kind), r.integer(length), r.nil()]
        }
        17..=19 => [random_cells(r, rng), random_object(r, rng), r.nil()],
        20 => {
            let tag = [r.nil(), r.symbol("rytest.tag")][rng.below(2)];
            [random_cells(r, rng), tag, r.nil()]
        }
        21..=24 => {
            let name = [
                "lazy", "forced", "active", "plain", "missing", "x", "absent",
            ];
            let env = if rng.chance(1, 4) {
                r.nil()
            } else {
                binding_env(r, rng.chance(1, 2))
            };
            [env, r.symbol(name[rng.below(name.len())]), r.nil()]
        }
        26 => {
            let object = random_object(r, rng);
            let length = match r.type_of(object) {
                LGLSXP | INTSXP | REALSXP | STRSXP | VECSXP => r.length(object) as c_int,
                _ => 0,
            };
            // Spare capacity around one chunk of 32 reaches both the chunked scan of a shrink and its
            // remainder.
            let spare = [0, 1, 31, 33, 80][rng.below(5)];
            let capacity = length + spare;
            let mut resize = || match rng.below(8) {
                0 => -1,
                1 => capacity + 1,
                _ => rng.below(capacity as usize + 1) as c_int,
            };
            let sizes = [spare, resize(), resize()];
            [object, integers(r, &sizes), r.nil()]
        }
        27 => [random_expression(r, rng, 0), r.nil(), r.nil()],
        _ => [
            random_object(r, rng),
            random_object(r, rng),
            random_object(r, rng),
        ],
    };
    Operation { code, arguments }
}

/// What native code hands `Rf_duplicate`: calls and pairlists with tagged and nested elements,
/// improper ends, attributes on a cell, and leaves from [`random_object`], plus a nesting deeper
/// than the direct path copies.
fn random_expression(r: &R, rng: &mut SplitMix64, depth: usize) -> Sexp {
    if depth == 0 && rng.chance(1, 16) {
        let mut call = r.symbol("x");
        for _ in 0..70 {
            call = r.call_expression("f", &[(None, call)]);
        }
        return call;
    }
    match rng.below(if depth < 3 { 6 } else { 2 }) {
        0 => r.symbol(["x", "y", "+"][rng.below(3)]),
        1 => random_object(r, rng),
        choice => {
            let mut cells = if rng.chance(1, 8) {
                r.integer(9)
            } else {
                r.nil()
            };
            for _ in 0..rng.below(4) {
                let element = random_expression(r, rng, depth + 1);
                cells = r.keep(unsafe { (r.api().Rf_cons)(element, cells) });
                if rng.chance(1, 3) {
                    unsafe { (r.api().SET_TAG)(cells, r.symbol("k")) };
                }
            }
            let cells = if choice == 2 {
                cells
            } else {
                r.keep(unsafe { (r.api().Rf_lcons)(r.symbol("f"), cells) })
            };
            if rng.chance(1, 8) && matches!(r.type_of(cells), LISTSXP | LANGSXP) {
                attach(r, cells, "rytest.tag", r.integer(1));
            }
            cells
        }
    }
}

/// Objects that reach every branch of the direct paths and the cases they decline: each vector
/// type with and without `dim`, `names` and `class`, ALTREP, pairlists, `NULL`, `CHARSXP`s and
/// symbols.
fn random_object(r: &R, rng: &mut SplitMix64) -> Sexp {
    let mut length = rng.below(7);
    let object = match rng.below(12) {
        0 => return r.nil(),
        1 => return r.symbol("rytest.symbol"),
        2 => return unsafe { (r.api().Rf_mkCharLenCE)(c"x".as_ptr(), 1, 1) },
        3 => {
            return r.keep(unsafe { (r.api().Rf_cons)(r.integer(1), r.nil()) });
        }
        4 => r
            .call(
                "rytest_sequence",
                &[r.integer(rng.below(9) as c_int), r.integer(length as c_int)],
            )
            .expect("an ALTREP sequence"),
        5 => {
            length = 1;
            r.real([0.5, -1e12, f64::NAN, 3e9][rng.below(4)])
        }
        choice => {
            let kind = [LGLSXP, INTSXP, REALSXP, STRSXP, VECSXP, INTSXP][choice - 6];
            let vector = r.keep(unsafe { (r.api().Rf_allocVector)(kind, length as isize) });
            fill(r, rng, vector, kind, length);
            vector
        }
    };
    let object = r.keep(object);
    if rng.chance(1, 3) && length > 0 {
        let rows = [1, length][rng.below(2)];
        let dim = integers(r, &[rows as c_int, (length / rows) as c_int]);
        let one = integers(r, &[length as c_int]);
        let one_dimensional = rng.chance(1, 3);
        attach(r, object, "dim", if one_dimensional { one } else { dim });
        // A one-dimensional array reports its dimnames as its names.
        if one_dimensional && rng.chance(1, 2) {
            let labels: Vec<String> = (0..length).map(|index| format!("d{index}")).collect();
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            let dimnames = r.keep(unsafe { (r.api().Rf_allocVector)(VECSXP, 1) });
            unsafe { (r.api().SET_VECTOR_ELT)(dimnames, 0, strings(r, &labels)) };
            attach(r, object, "dimnames", dimnames);
        }
    }
    if rng.chance(1, 5) {
        // Compact row names expand on read; stored ones come back as they are.
        let rows = match rng.below(3) {
            0 => integers(r, &[i32::MIN, -(length as c_int)]),
            1 => integers(r, &[3, 1]),
            _ => strings(r, &["r1", "r2"]),
        };
        attach(r, object, "row.names", rows);
    }
    if rng.chance(1, 4) {
        let names: Vec<String> = (0..length).map(|index| format!("n{index}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        attach(r, object, "names", strings(r, &names));
    }
    if rng.chance(1, 4) {
        attach(
            r,
            object,
            "class",
            strings(r, &[["first", "second"][rng.below(2)], "rytest_class"]),
        );
    }
    object
}

/// What the cell writers get: pairlists and calls of one to three cells, which they write to,
/// and `NULL` and a vector, which they refuse.
fn random_cells(r: &R, rng: &mut SplitMix64) -> Sexp {
    let mut cells = r.nil();
    let length = rng.below(4);
    for index in 0..length {
        let car = r.integer(index as c_int);
        cells = r.keep(unsafe {
            if index + 1 == length && rng.chance(1, 2) {
                (r.api().Rf_lcons)(r.symbol("c"), cells)
            } else {
                (r.api().Rf_cons)(car, cells)
            }
        });
    }
    if length == 0 && rng.chance(1, 2) {
        return r.integer(1);
    }
    cells
}

/// An environment holding every kind of binding the binding readers tell apart, or the frame of
/// a call whose argument `x` is an unforced promise.
fn binding_env(r: &R, call_frame: bool) -> Sexp {
    let code = if call_frame {
        "(function(x) environment())(1 + 1)"
    } else {
        "local({
            e <- new.env()
            delayedAssign('lazy', 1 + 2, assign.env = e)
            delayedAssign('forced', 3, assign.env = e)
            get('forced', envir = e)
            makeActiveBinding('active', function() 1, e)
            assign('plain', 1, envir = e)
            assign('missing', quote(expr = ), envir = e)
            e
        })"
    };
    let parsed = r
        .eval("parse", &[(Some("text"), r.string(code))])
        .expect("the code parses");
    r.keep(
        r.eval_expression(r.element(parsed, 0))
            .expect("the environment builds"),
    )
}

fn fill(r: &R, rng: &mut SplitMix64, vector: Sexp, kind: c_int, length: usize) {
    unsafe {
        match kind {
            REALSXP => std::slice::from_raw_parts_mut((r.api().REAL)(vector), length)
                .iter_mut()
                .for_each(|value| *value = [0.0, 1.5, f64::NAN, -4e9][rng.below(4)]),
            STRSXP => {
                for index in 0..length {
                    let text = ["a", "factor", "TRUE", "12"][rng.below(4)];
                    let element =
                        (r.api().Rf_mkCharLenCE)(text.as_ptr().cast(), text.len() as c_int, 1);
                    (r.api().SET_STRING_ELT)(vector, index as isize, element);
                }
            }
            VECSXP => {
                for index in 0..length {
                    let element = [r.nil(), r.integer(index as c_int), r.string("a")][rng.below(3)];
                    (r.api().SET_VECTOR_ELT)(vector, index as isize, element);
                }
            }
            _ => std::slice::from_raw_parts_mut((r.api().INTEGER)(vector), length)
                .iter_mut()
                .for_each(|value| *value = [0, 1, i32::MIN, -3][rng.below(4)]),
        }
    }
}

fn random_name(r: &R, rng: &mut SplitMix64) -> Sexp {
    let names = [
        "names",
        "dim",
        "dimnames",
        "class",
        "row.names",
        "levels",
        "rytest.tag",
    ];
    let name = names[rng.below(names.len())];
    match rng.below(8) {
        0 => r.string(name),
        1 => r.integer(1),
        _ => r.symbol(name),
    }
}

/// A value to set: `NULL`, extents that fit or do not, `dimnames` lists that the direct path
/// takes or declines, classes with and without `factor`, names of every length, or anything.
fn random_value(r: &R, rng: &mut SplitMix64, object: Sexp) -> Sexp {
    let length = if matches!(
        r.type_of(object),
        LGLSXP | INTSXP | REALSXP | STRSXP | VECSXP
    ) {
        r.length(object) as c_int
    } else {
        1
    };
    match rng.below(9) {
        0 => r.nil(),
        1 => {
            let extents = [
                vec![length],
                vec![1, length],
                vec![length, 1],
                vec![],
                vec![i32::MIN],
                vec![-1, -length],
                vec![length + 1],
            ];
            let extents = &extents[rng.below(extents.len())];
            let value = integers(r, extents);
            if rng.chance(1, 5) {
                attach(r, value, "names", strings(r, &vec!["e"; extents.len()]));
            }
            value
        }
        2 => r.real(length as f64),
        3 | 4 => {
            let components = 1 + rng.below(3);
            let list = r.keep(unsafe { (r.api().Rf_allocVector)(VECSXP, components as isize) });
            for index in 0..components {
                let labels: Vec<String> = (0..[length as usize, 0, 1, 2][rng.below(4)])
                    .map(|label| format!("l{label}"))
                    .collect();
                let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
                let component = match rng.below(5) {
                    0 => r.nil(),
                    1 => integers(r, &[1, 2]),
                    _ => strings(r, &labels),
                };
                unsafe { (r.api().SET_VECTOR_ELT)(list, index as isize, component) };
            }
            if rng.chance(1, 5) {
                attach(r, list, "names", strings(r, &vec!["d"; components]));
            }
            list
        }
        5 => strings(r, &[["first", "factor", ""][rng.below(3)], "rytest_class"]),
        6 => r.keep(unsafe { (r.api().Rf_allocVector)(STRSXP, 0) }),
        7 => {
            let names: Vec<String> = (0..[length, length - 1, length + 1][rng.below(3)].max(0))
                .map(|index| format!("v{index}"))
                .collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            strings(r, &names)
        }
        _ => random_object(r, rng),
    }
}

fn attach(r: &R, object: Sexp, name: &str, value: Sexp) {
    // Building the inputs goes through the slow path, so a direct path bug cannot hide in them.
    unsafe {
        (r.api().ry_set_slow_paths_only)(1);
        (r.api().Rf_setAttrib)(object, r.symbol(name), value);
        (r.api().ry_set_slow_paths_only)(0);
    }
}

fn integers(r: &R, values: &[c_int]) -> Sexp {
    let vector = r.keep(unsafe { (r.api().Rf_allocVector)(INTSXP, values.len() as isize) });
    unsafe {
        std::slice::from_raw_parts_mut((r.api().INTEGER)(vector), values.len())
            .copy_from_slice(values)
    };
    vector
}

fn strings(r: &R, values: &[&str]) -> Sexp {
    let vector = r.keep(unsafe { (r.api().Rf_allocVector)(STRSXP, values.len() as isize) });
    for (index, text) in values.iter().enumerate() {
        unsafe {
            let element = (r.api().Rf_mkCharLenCE)(text.as_ptr().cast(), text.len() as c_int, 1);
            (r.api().SET_STRING_ELT)(vector, index as isize, element);
        }
    }
    vector
}

/// Calls each `bench_` loop in `rytest` makes per round.
const BENCH_CALLS: u64 = 4000;

/// The most instructions one call of each hot API function may cost, measured with callgrind
/// over a `bench_` loop in `rytest` (`rytest_api_bench`), loop and setup included. Each budget
/// sits at or below the cost of the same loop with every direct path switched off, which is the
/// entry point's general path, so none can get slower than that without failing here. Running
/// this test with `RY_API_BENCH_SLOW=1` measures that general path and fails with its costs.
const BUDGETS: &[(&str, u64)] = &[
    ("bench_set_attrib_symbol", 136),
    ("bench_set_attrib_class", 159),
    ("bench_set_attrib_dim", 203),
    ("bench_set_attrib_dimnames", 600),
    ("bench_set_attrib_names", 137),
    ("bench_get_attrib", 80),
    ("bench_alloc_vector", 260),
    ("bench_scalar_integer", 240),
    ("bench_scalar_logical", 94),
    ("bench_mk_string", 428),
    ("bench_install", 375),
    ("bench_nrows_ncols", 119),
    ("bench_lang2", 200),
    ("bench_cons", 129),
    ("bench_as_scalars", 193),
    ("bench_inherits", 149),
    ("bench_protect_with_index", 38),
    ("bench_char_is_ascii", 33),
    ("bench_external_ptr_addr", 15),
    ("bench_get_option", 160),
    ("bench_class_names_gets", 280),
    ("bench_set_car_cdr", 178),
    ("bench_binding_probes", 203),
    ("bench_delayed_binding", 230),
    ("bench_resizable_grow_shrink", 6_600),
    ("bench_duplicate_call", 2_100),
];

/// Runs the `bench_` loops under callgrind in a child copy of this test and checks the cost per
/// call against [`BUDGETS`]. The counts mean something only for an optimized libR and need
/// valgrind, so a debug build or a machine without valgrind skips the check.
#[test]
fn hot_api_functions_stay_within_instruction_budgets() {
    if std::env::var_os("RY_API_BENCH_CHILD").is_some() {
        let r = r_with_base();
        rytest(&r);
        let env = binding_env(&r, false);
        if std::env::var_os("RY_API_BENCH_SLOW").is_some() {
            unsafe { (r.api().ry_set_slow_paths_only)(1) };
        }
        r.call("rytest_api_bench", &[r.integer(BENCH_CALLS as c_int), env])
            .expect("the benchmark loops run");
        return;
    }
    if cfg!(debug_assertions) {
        eprintln!("skipped: instruction counts need an optimized build");
        return;
    }
    if Command::new("valgrind").arg("--version").output().is_err() {
        eprintln!("skipped: valgrind is not installed");
        return;
    }
    let output = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("api-bench.callgrind");
    let executable = std::env::current_exe().expect("the test executable has a path");
    let status = Command::new("valgrind")
        .args([
            "--tool=callgrind",
            "--collect-atstart=no",
            "--toggle-collect=bench_*",
            &format!("--callgrind-out-file={}", output.display()),
        ])
        .arg(&executable)
        .args([
            "--exact",
            "hot_api_functions_stay_within_instruction_budgets",
            "--test-threads=1",
        ])
        .env("RY_API_BENCH_CHILD", "1")
        .output()
        .expect("valgrind runs");
    assert!(
        status.status.success(),
        "the benchmark child failed:\n{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let annotated = Command::new("callgrind_annotate")
        .args(["--inclusive=yes", "--threshold=100"])
        .arg(&output)
        .output()
        .expect("callgrind_annotate runs");
    let report = String::from_utf8_lossy(&annotated.stdout);
    let mut failures = Vec::new();
    for &(function, budget) in BUDGETS {
        let cost = report
            .lines()
            .find(|line| line.contains(&format!(":{function} ")))
            .and_then(|line| line.split_whitespace().next())
            .and_then(|count| count.replace(',', "").parse::<u64>().ok())
            .unwrap_or_else(|| panic!("callgrind reported no cost for {function}:\n{report}"));
        let per_call = cost / BENCH_CALLS;
        eprintln!("{function}: {per_call} instructions per call (budget {budget})");
        if per_call > budget {
            failures.push(format!("{function}: {per_call} > {budget}"));
        }
    }
    assert!(failures.is_empty(), "over budget:\n{}", failures.join("\n"));
}
