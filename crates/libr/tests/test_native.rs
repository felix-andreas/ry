//! Real native packages built against ry's headers: brio from the benchmark closure and the
//! purpose-written `rytest`, called through `.Call` with correct results, including under
//! collection stress, with PROTECT imbalance, and with errors unwinding through native frames.

mod harness;

use harness::*;
use std::{
    ffi::{CString, c_char, c_int, c_void},
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

#[test]
fn brio_built_against_ry_headers_reads_and_writes_files() {
    let r = r();
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    let library = BUILT.get_or_init(|| build_package("brio", Headers::Ry));
    r.load(library);
    brio_round_trip(&r);
    r.stress(true);
    brio_round_trip(&r);
}

#[test]
fn routines_return_correct_values() {
    let r = r();
    rytest(&r);
    let five = r.integer(5);
    let list = r
        .call("rytest_make_list", &[five])
        .expect("rytest_make_list");
    assert_eq!(r.type_of(list), VECSXP);
    assert_eq!(r.length(list), 5);
    assert_eq!(r.strings(r.element(list, 4)), vec![Some("item_5".into())]);
    let names = unsafe { (r.api().Rf_getAttrib)(list, r.data("R_NamesSymbol")) };
    assert_eq!(r.strings(names)[0], Some("n1".into()));

    let matrix = r
        .call("rytest_matrix", &[r.integer(2), r.integer(3)])
        .expect("rytest_matrix");
    assert_eq!(r.integers(matrix), vec![2, 3, 1, 1]);

    let environment = r
        .call("rytest_environment", &[])
        .expect("rytest_environment");
    assert_eq!(r.integers(environment), vec![7, 0, 8, 1, 1]);

    let strings = r.call("rytest_strings", &[]).expect("rytest_strings");
    assert_eq!(
        r.strings(strings),
        vec![
            Some("caf\u{e9}".into()),
            Some("latin1".into()),
            Some("ascii".into()),
            Some("interned".into()),
            None,
            Some("trunc".into())
        ]
    );
    let error = r
        .call("rytest_embedded_nul", &[])
        .expect_err("an embedded NUL is an error");
    assert_eq!(error, "Error: embedded nul in string: 'a\\0b'\n");

    let callable = r
        .call("rytest_call_callable", &[r.real(21.0)])
        .expect("a registered C callable");
    assert_eq!(r.reals(callable), vec![42.0]);
    let error = r
        .call("rytest_missing_callable", &[])
        .expect_err("a missing callable");
    assert_eq!(
        error,
        "Error: function 'nothing' not provided by package 'nopkg'\n"
    );

    let arity = r
        .call("rytest_sum", &[])
        .expect_err("a registered arity is checked");
    assert!(
        arity.contains("Incorrect number of arguments (0), expecting 1 for 'rytest_sum'"),
        "{arity}"
    );
    let unknown = r
        .call("no_such_routine", &[])
        .expect_err("an unknown routine");
    assert_eq!(
        unknown,
        "Error in .Call(\"no_such_routine\") : \n  C symbol name \"no_such_routine\" not in load table\n"
    );
}

#[test]
fn coercion_follows_r() {
    let r = r();
    rytest(&r);
    let texts = r.keep(unsafe { (r.api().Rf_allocVector)(STRSXP, 5) });
    for (index, text) in ["1.5", "  2e3 ", "abc", "NA", "0x1F"].iter().enumerate() {
        let element = unsafe {
            (r.api().Rf_mkCharLenCE)(text.as_ptr() as *const c_char, text.len() as c_int, 0)
        };
        unsafe { (r.api().SET_STRING_ELT)(texts, index as isize, element) };
    }
    let reals = r
        .call("rytest_coerce", &[texts, r.integer(REALSXP)])
        .expect("coerce to double");
    let values = r.reals(reals);
    assert_eq!(values[0], 1.5);
    assert_eq!(values[1], 2000.0);
    assert!(values[2].is_nan() && values[3].is_nan());
    assert_eq!(values[4], 31.0);
    assert_eq!(r.warnings(), vec!["NAs introduced by coercion".to_owned()]);

    let doubles = r.keep(unsafe { (r.api().Rf_allocVector)(REALSXP, 7) });
    unsafe { std::slice::from_raw_parts_mut((r.api().REAL)(doubles), 7) }.copy_from_slice(&[
        0.1,
        100000.0,
        123456.0,
        1.0 / 3.0,
        1e-5,
        -2.5,
        1e15,
    ]);
    let strings = r
        .call("rytest_coerce", &[doubles, r.integer(STRSXP)])
        .expect("coerce to character");
    assert_eq!(
        r.strings(strings),
        [
            "0.1",
            "1e+05",
            "123456",
            "0.333333333333333",
            "1e-05",
            "-2.5",
            "1e+15"
        ]
        .map(|text| Some(text.to_owned()))
        .to_vec()
    );
    let big = r.real(3e10);
    let integers = r
        .call("rytest_coerce", &[big, r.integer(INTSXP)])
        .expect("coerce to integer");
    assert_eq!(r.integers(integers), vec![i32::MIN]);
    assert!(
        r.warnings()
            .contains(&"NAs introduced by coercion to integer range".to_owned())
    );
}

#[test]
fn errors_unwind_through_native_frames() {
    let r = r();
    rytest(&r);
    let baseline_contexts = unsafe { (r.api().ry_context_depth)() };
    let baseline_depth = r.runtime_protect_depth();
    let message = r.string("inner");
    let error = r
        .call("rytest_error", &[message])
        .expect_err("Rf_error unwinds");
    assert_eq!(error, "Error: custom: inner\n");
    let error = r
        .call("rytest_errorcall", &[])
        .expect_err("Rf_errorcall unwinds");
    assert_eq!(error, "Error in f(1L) : boom\n");

    // An error two native frames deep: rytest_eval -> Rf_eval -> .Call -> rytest_error.
    let inner_call = r.call_expression(
        ".Call",
        &[(None, r.string("rytest_error")), (None, message)],
    );
    let inner = r.quote(inner_call);
    let global: Sexp = r.data("R_GlobalEnv");
    let error = r
        .call("rytest_eval", &[inner, global])
        .expect_err("a nested error reaches the outer call");
    let inner = inner_call;
    let inner = r.quote(inner);
    assert_eq!(error, "Error: custom: inner\n");

    // A nested R error (stop) inside native code, stopped by R_tryEvalSilent in native code.
    let stop = r.call_expression("stop", &[(None, r.string("from stop"))]);
    let stop = r.quote(stop);
    let caught = r
        .call("rytest_try_eval", &[stop])
        .expect("R_tryEvalSilent stops the error");
    assert_eq!(r.strings(caught), vec![Some("Error: from stop\n".into())]);

    // R_ToplevelExec reports and stops the error, then returns FALSE.
    let toplevel = r
        .call("rytest_toplevel", &[inner])
        .expect("R_ToplevelExec returns");
    assert_eq!(r.integers(toplevel), vec![0]);
    assert!(
        String::from_utf8_lossy(&CONSOLE.lock().expect("console")).contains("Error: custom: inner")
    );
    let fine = r.call_expression("identity", &[(None, r.integer(3))]);
    let fine = r.quote(fine);
    assert_eq!(
        r.integers(r.call("rytest_toplevel", &[fine]).expect("toplevel")),
        vec![1]
    );

    // R_tryCatchError hands the handler the condition object.
    let handled = r
        .call("rytest_try_catch", &[inner])
        .expect("the handler's value");
    assert_eq!(
        r.strings(handled),
        vec![Some("caught: custom: inner".into())]
    );

    // R_UnwindProtect runs its cleanup with jump = TRUE and then continues the unwind.
    let before = r.integers(r.call("rytest_cleanup_state", &[]).expect("state"))[0];
    let error = r
        .call("rytest_unwind_protect", &[inner])
        .expect_err("the unwind continues");
    assert_eq!(error, "Error: custom: inner\n");
    assert_eq!(
        r.integers(r.call("rytest_cleanup_state", &[]).expect("state")),
        vec![before + 1, 1]
    );
    let value = r.call("rytest_unwind_protect", &[fine]).expect("no unwind");
    assert_eq!(r.integers(value), vec![3]);
    assert_eq!(
        r.integers(r.call("rytest_cleanup_state", &[]).expect("state")),
        vec![before + 2, 0]
    );

    // R_ExecWithCleanup runs the cleanup on both paths.
    r.call("rytest_exec_with_cleanup", &[inner])
        .expect_err("the error continues");
    r.call("rytest_exec_with_cleanup", &[fine])
        .expect("no error");
    assert_eq!(
        r.integers(r.call("rytest_cleanup_state", &[]).expect("state"))[0],
        before + 4
    );

    // C++ destructors run when the error unwinds through C++ frames, cpp11-style.
    let destructors = r.integers(r.call("rytest_destructor_count", &[]).expect("count"))[0];
    let error = r
        .call("rytest_cpp_unwind", &[inner])
        .expect_err("the error continues past C++");
    assert_eq!(error, "Error: custom: inner\n");
    assert_eq!(
        r.integers(r.call("rytest_destructor_count", &[]).expect("count"))[0],
        destructors + 1
    );

    assert_eq!(
        unsafe { (r.api().ry_context_depth)() },
        baseline_contexts,
        "every C context was popped"
    );
    assert_eq!(
        r.runtime_protect_depth(),
        baseline_depth,
        "every jump restored the PROTECT stack"
    );
}

#[test]
fn protect_imbalance_is_reported_and_repaired() {
    let r = r();
    rytest(&r);
    let depth = r.runtime_protect_depth();
    let value = r
        .call("rytest_protect_leak", &[])
        .expect("a leak is not an error");
    assert_eq!(r.integers(value), vec![1]);
    assert_eq!(
        r.runtime_protect_depth(),
        depth,
        "the leaked protections are dropped"
    );
    let warnings = r.warnings();
    let numbers: Vec<c_int> = warnings
        .first()
        .and_then(|warning| warning.strip_prefix("stack imbalance in '.Call', "))
        .map(|rest| {
            rest.split(" then ")
                .filter_map(|number| number.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        matches!(numbers.as_slice(), [entry, exit] if exit - entry == 2),
        "{warnings:?}"
    );

    let error = r
        .call("rytest_unprotect_too_many", &[])
        .expect_err("unprotecting the caller's objects");
    assert_eq!(
        error,
        "Error: unprotect(): only 1 protected items, can't unprotect 2\n"
    );
    let error = r
        .call("rytest_protect_overflow", &[])
        .expect_err("the stack has a limit");
    assert_eq!(error, "Error: protect(): protection stack overflow\n");
    let error = r
        .call("rytest_protect_depth_error", &[])
        .expect_err("an error");
    assert_eq!(error, "Error: failing with three objects protected\n");
    assert_eq!(r.runtime_protect_depth(), depth);
}

#[test]
fn r_alloc_memory_is_released_on_return_and_on_error() {
    let r = r();
    rytest(&r);
    let baseline = unsafe { (r.api().ry_arena_bytes)() };
    let sum = r
        .call("rytest_r_alloc", &[r.integer(1000)])
        .expect("rytest_r_alloc");
    assert_eq!(r.reals(sum), vec![499500.0]);
    assert_eq!(
        unsafe { (r.api().ry_arena_bytes)() },
        baseline,
        "released when .Call returns"
    );
    let error = r
        .call("rytest_r_alloc_error", &[r.integer(100_000)])
        .expect_err("an error");
    assert_eq!(error, "Error: failing while holding R_alloc memory\n");
    assert_eq!(
        unsafe { (r.api().ry_arena_bytes)() },
        baseline,
        "released by the jump"
    );
}

#[test]
fn objects_survive_collection_and_finalizers_run() {
    let r = r();
    rytest(&r);
    for stress in [false, true] {
        r.stress(stress);
        let churn = r
            .call(
                "rytest_gc_churn",
                &[r.integer(if stress { 120 } else { 2000 })],
            )
            .expect("rytest_gc_churn");
        let n = if stress { 120.0 } else { 2000.0 };
        assert_eq!(r.reals(churn), vec![3.0 * n * (n - 1.0) / 2.0]);
        let temporaries = r
            .call("rytest_unprotected_temporaries", &[r.integer(500)])
            .expect("handle scope");
        assert_eq!(r.integers(r.element(temporaries, 0)), vec![42]);
        assert_eq!(
            r.strings(r.element(temporaries, 1)),
            vec![Some("kept".into())]
        );
    }
    r.stress(false);
    assert!(r.stats().major_collections > 0);

    let before = r.integers(r.call("rytest_finalized", &[]).expect("count"))[0];
    let pointer = r
        .call("rytest_external_ptr", &[r.integer(5)])
        .expect("an external pointer");
    assert_eq!(r.type_of(pointer), EXTPTRSXP);
    assert_eq!(
        r.integers(r.call("rytest_external_value", &[pointer]).expect("value")),
        vec![5]
    );
    unsafe { (r.api().ry_collect)(1) };
    assert_eq!(
        r.integers(r.call("rytest_finalized", &[]).expect("count"))[0],
        before,
        "still reachable"
    );
    // Drop the only reference: the harness's own protection of the pointer and its call.
    unsafe { (r.api().Rf_unprotect)(r.protected.get()) };
    r.protected.set(0);
    unsafe { (r.api().ry_collect)(1) };
    assert_eq!(
        r.integers(r.call("rytest_finalized", &[]).expect("count"))[0],
        before + 5
    );
}

#[test]
fn altrep_objects_dispatch_to_their_class() {
    let r = r();
    rytest(&r);
    let sequence = r
        .call("rytest_sequence", &[r.integer(10), r.integer(5)])
        .expect("an ALTREP sequence");
    assert_eq!(r.type_of(sequence), INTSXP);
    assert_eq!(r.length(sequence), 5);
    let info = r.call("rytest_sequence_info", &[sequence]).expect("info");
    assert_eq!(r.integers(info), vec![1, 0, 12, 1, 14, 1]);
    let sum = r
        .call("rytest_sum", &[sequence])
        .expect("INTEGER_RO on ALTREP");
    assert_eq!(r.reals(sum), vec![60.0]);
    let copy = r
        .call("rytest_duplicate", &[sequence])
        .expect("duplicate materializes");
    assert_eq!(r.integers(r.element(copy, 1)), vec![10, 11, 12, 13, 14]);
    let strings = r
        .call("rytest_coerce", &[sequence, r.integer(STRSXP)])
        .expect("coerce ALTREP");
    assert_eq!(r.strings(strings)[4], Some("14".into()));
}

#[test]
fn duplicate_and_identical() {
    let r = r();
    rytest(&r);
    let values = r.keep(unsafe { (r.api().Rf_allocVector)(REALSXP, 3) });
    unsafe { std::slice::from_raw_parts_mut((r.api().REAL)(values), 3) }
        .copy_from_slice(&[1.0, 2.0, 3.0]);
    let pair = r.call("rytest_duplicate", &[values]).expect("duplicate");
    assert_eq!(
        r.reals(r.element(pair, 0)),
        vec![1.0, 2.0, 3.0],
        "the original is untouched"
    );
    assert_eq!(r.reals(r.element(pair, 1)), vec![-1.0, 2.0, 3.0]);
    let same = r
        .call("rytest_identical", &[values, r.element(pair, 0)])
        .expect("identical");
    assert_eq!(r.integers(same), vec![1]);
    let different = r
        .call("rytest_identical", &[values, r.element(pair, 1)])
        .expect("identical");
    assert_eq!(r.integers(different), vec![0]);
}

#[test]
fn warnings_are_collected() {
    let r = r();
    rytest(&r);
    let value = r
        .call("rytest_warning", &[])
        .expect("a warning does not unwind");
    assert_eq!(r.integers(value), vec![1]);
    assert_eq!(r.warnings(), vec!["careful: 42".to_owned()]);
}

/// The generator against GNU R's own output for `set.seed(42)`.
#[test]
fn the_random_number_generator_matches_gnu_r() {
    let r = r();
    rytest(&r);
    unsafe { (r.api().ry_set_seed)(42) };
    let uniform = r.call("rytest_unif", &[r.integer(5)]).expect("unif_rand");
    unsafe { (r.api().ry_set_seed)(42) };
    let sample = r
        .call("rytest_sample", &[r.real(10.0), r.integer(8)])
        .expect("R_unif_index");
    let uniform = r.reals(uniform);
    let sample = r.integers(sample);
    if !gnu_r_available() {
        return;
    }
    let output = Command::new("Rscript")
        .args([
            "-e",
            "set.seed(42); cat(sprintf('%.17g', runif(5)), sep = '\\n'); set.seed(42); cat(sample.int(10, 8, replace = TRUE), sep = '\\n')",
        ])
        .output()
        .expect("Rscript");
    let expected: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    let expected_uniform: Vec<f64> = expected[..5]
        .iter()
        .map(|line| line.parse().expect("a double"))
        .collect();
    let expected_sample: Vec<c_int> = expected[5..]
        .iter()
        .map(|line| line.parse().expect("an integer"))
        .collect();
    assert_eq!(uniform, expected_uniform);
    assert_eq!(sample, expected_sample);
}

/// Every function and variable the headers declare is exported, so a package that compiles
/// against them also links.
#[test]
fn every_declared_name_is_exported() {
    let r = r();
    let include = Path::new(env!("CARGO_MANIFEST_DIR")).join("include");
    let mut headers = vec![
        include.join("R.h"),
        include.join("Rinternals.h"),
        include.join("ry_embed.h"),
    ];
    for entry in std::fs::read_dir(include.join("R_ext")).expect("R_ext") {
        headers.push(entry.expect("an entry").path());
    }
    let mut missing = Vec::new();
    let mut checked = 0;
    for header in headers {
        let text = std::fs::read_to_string(&header).expect("a header");
        // One declaration per statement: a declaration may span lines, and preprocessor lines
        // (with their continuations) are not declarations.
        let mut statements = Vec::new();
        let mut current = String::new();
        let mut in_directive = false;
        for line in text.lines() {
            let line = line.trim();
            if in_directive || line.starts_with('#') {
                in_directive = line.ends_with('\\');
                continue;
            }
            if current.is_empty() && (line.starts_with("/*") || line.starts_with("//")) {
                continue;
            }
            current.push_str(line);
            current.push(' ');
            if line.ends_with(';') || line.ends_with('{') || line.ends_with('}') {
                statements.push(std::mem::take(&mut current));
            }
        }
        // Members of a struct body (the stream structs' callback fields) are not declarations.
        let mut in_struct = false;
        for statement in statements {
            let line = statement.trim();
            if line.starts_with("struct ") && line.ends_with('{') {
                in_struct = true;
                continue;
            }
            if line.starts_with('}') {
                in_struct = false;
                continue;
            }
            if in_struct || line.starts_with("typedef") || !line.ends_with(';') {
                continue;
            }
            let name = if let Some(variable) = line.strip_prefix("LibExtern ") {
                variable
                    .trim_end_matches(';')
                    .rsplit(' ')
                    .next()
                    .map(str::to_owned)
            } else {
                declared_function(line)
            };
            let Some(name) = name else { continue };
            if name.is_empty() || name == "void" {
                continue;
            }
            checked += 1;
            let symbol = CString::new(name.clone()).expect("no NUL");
            if unsafe {
                r.api()
                    .library
                    .get::<*mut c_void>(symbol.as_bytes_with_nul())
            }
            .is_err()
            {
                missing.push(name);
            }
        }
    }
    assert!(
        checked > 300,
        "the header scan found only {checked} declarations"
    );
    assert!(missing.is_empty(), "declared but not exported: {missing:?}");
}

/// The name a function declaration declares: the identifier before its parameter list, which
/// may sit in parentheses (`int (TYPEOF)(SEXP x);`) to keep it safe from macros.
fn declared_function(statement: &str) -> Option<String> {
    let mut text = statement.trim_end_matches(';').trim_end();
    if let Some(position) = text.find(" R_PRINTF_FORMAT") {
        text = text[..position].trim_end();
    }
    if !text.ends_with(')') {
        return None;
    }
    let mut depth = 0;
    let mut open = None;
    for (position, character) in text.char_indices().rev() {
        match character {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    open = Some(position);
                    break;
                }
            }
            _ => {}
        }
    }
    let head = text[..open?].trim_end().trim_end_matches(')');
    let name: String = head
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let name: String = name.chars().rev().collect();
    let has_type = head.len() > name.len();
    (!name.is_empty() && has_type).then_some(name)
}

/// The coverage of the API the benchmark closure uses, measured in `tools/research/data`.
#[test]
fn the_most_used_api_names_are_exported() {
    let r = r();
    let table = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/research/data/symbols.csv"),
    )
    .expect("the measured symbol table");
    let macros: &[(&str, Option<&str>)] = &[
        ("PROTECT", Some("Rf_protect")),
        ("UNPROTECT", Some("Rf_unprotect")),
        ("UNPROTECT_PTR", Some("Rf_unprotect_ptr")),
        ("PROTECT_WITH_INDEX", Some("R_ProtectWithIndex")),
        ("REPROTECT", Some("R_Reprotect")),
        ("CHAR", Some("R_CHAR")),
        ("R_Calloc", Some("R_chk_calloc")),
        ("R_Free", Some("R_chk_free")),
        ("R_Realloc", Some("R_chk_realloc")),
        ("MAYBE_REFERENCED", Some("NAMED")),
        ("MAYBE_SHARED", Some("NAMED")),
        ("NA_STRING", Some("R_NaString")),
        ("NA_INTEGER", Some("R_NaInt")),
        ("NA_REAL", Some("R_NaReal")),
        ("NA_LOGICAL", Some("R_NaInt")),
        ("ISNA", Some("R_IsNA")),
        ("ISNAN", Some("R_isnancpp")),
        ("R_FINITE", Some("R_finite")),
        ("LCONS", Some("Rf_lcons")),
        ("FALSE", None),
        ("TRUE", None),
        ("R_Version", None),
        ("R_VERSION", None),
        ("R_INLINE", None),
        ("M_PI", None),
    ];
    let mut rows: Vec<(String, usize)> = table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.splitn(6, ',').collect();
            let class = *fields.get(1)?;
            matches!(class, "api" | "experimental" | "non-api" | "embedding")
                .then(|| Some((fields[0].to_owned(), fields.get(3)?.parse().ok()?)))?
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.1));
    let mut missing = Vec::new();
    for (name, _) in rows.iter().take(150) {
        let target = macros
            .iter()
            .find(|(macro_name, _)| macro_name == name)
            .map_or(Some(name.as_str()), |(_, target)| *target);
        let Some(target) = target else { continue };
        let symbol = CString::new(target).expect("no NUL");
        if unsafe {
            r.api()
                .library
                .get::<*mut c_void>(symbol.as_bytes_with_nul())
        }
        .is_err()
        {
            missing.push(name.clone());
        }
    }
    assert!(
        missing.len() <= 2,
        "the most-used names that libR lacks: {missing:?}"
    );
}

/// Inputs GNU R accepts that ry once mishandled: escaped temporaries in the next collection, a
/// routine that returns C `NULL`, a non-list `CDR`, and `finally` when a handler raises.
#[test]
fn edge_cases_gnu_r_accepts() {
    let r = r();
    rytest(&r);
    r.call("rytest_escaped_temporary", &[])
        .expect("escaped temporaries");
    unsafe {
        (r.api().ry_collect)(0);
        (r.api().ry_collect)(0);
        (r.api().ry_collect)(1);
    }
    let value = r.call("rytest_return_null", &[]).expect("a NULL return");
    assert_eq!(value, r.nil());
    assert_eq!(
        r.warnings(),
        vec!["converting NULL pointer to R NULL".to_owned()]
    );
    let tail = r
        .call("rytest_setcdr_any", &[])
        .expect("SETCDR with a vector");
    assert_eq!(r.integers(tail), vec![2]);

    let message = r.string("inner");
    let inner = r.call_expression(
        ".Call",
        &[(None, r.string("rytest_error")), (None, message)],
    );
    let inner = r.quote(inner);
    let before = r.integers(r.call("rytest_cleanup_state", &[]).expect("state"))[0];
    let error = r
        .call("rytest_try_catch_failing_handler", &[inner])
        .expect_err("the handler's error continues");
    assert_eq!(error, "Error: from the handler\n");
    assert_eq!(
        r.integers(r.call("rytest_cleanup_state", &[]).expect("state"))[0],
        before + 1
    );
}
