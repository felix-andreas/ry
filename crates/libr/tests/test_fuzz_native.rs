//! Fuzzing the native boundary and the API against models. One generator nests every
//! non-local-exit construct native code can set up (Rf_eval, R_tryEval, R_ToplevelExec,
//! R_tryCatchError, R_UnwindProtect, R_ExecWithCleanup and a cpp11-style C++ unwind) around
//! values and errors, and checks the outcome, the cleanups that ran, and that every C context,
//! protection and `R_alloc` block was released. The other drives random API sequences under
//! collection pressure and checks every stored object against a model. `FUZZ_ITERS` scales both.

mod harness;

use {
    harness::*,
    std::ffi::c_int,
    syntax::testing::{SplitMix64, fuzz_iterations},
};

#[derive(Clone, Debug)]
enum Expression {
    Value(c_int),
    Error(String),
    Eval(Box<Expression>),
    TryEval(Box<Expression>),
    Toplevel(Box<Expression>),
    TryCatch(Box<Expression>),
    UnwindProtect(Box<Expression>),
    ExecWithCleanup(Box<Expression>),
    CppUnwind(Box<Expression>),
}

#[derive(Clone, Debug, PartialEq)]
enum Outcome {
    Integer(c_int),
    Text(String),
    Logical(bool),
    Error(String),
}

#[derive(Default, Debug, PartialEq)]
struct Effects {
    cleanups: c_int,
    destructors: c_int,
}

fn generate(rng: &mut SplitMix64, depth: usize) -> Expression {
    if depth == 0 || rng.chance(1, 4) {
        return if rng.chance(1, 2) {
            Expression::Value(rng.below(1000) as c_int)
        } else {
            Expression::Error(format!("e{}", rng.below(100)))
        };
    }
    let inner = Box::new(generate(rng, depth - 1));
    match rng.below(7) {
        0 => Expression::Eval(inner),
        1 => Expression::TryEval(inner),
        2 => Expression::Toplevel(inner),
        3 => Expression::TryCatch(inner),
        4 => Expression::UnwindProtect(inner),
        5 => Expression::ExecWithCleanup(inner),
        _ => Expression::CppUnwind(inner),
    }
}

/// What R does with the expression.
fn model(expression: &Expression, effects: &mut Effects) -> Outcome {
    match expression {
        Expression::Value(value) => Outcome::Integer(*value),
        Expression::Error(message) => Outcome::Error(format!("custom: {message}")),
        Expression::Eval(inner) => model(inner, effects),
        Expression::TryEval(inner) => match model(inner, effects) {
            Outcome::Error(message) => Outcome::Text(format!("Error: {message}\n")),
            other => other,
        },
        Expression::Toplevel(inner) => {
            Outcome::Logical(!matches!(model(inner, effects), Outcome::Error(_)))
        }
        Expression::TryCatch(inner) => match model(inner, effects) {
            Outcome::Error(message) => Outcome::Text(format!("caught: {message}")),
            other => other,
        },
        Expression::UnwindProtect(inner) | Expression::ExecWithCleanup(inner) => {
            let outcome = model(inner, effects);
            effects.cleanups += 1;
            outcome
        }
        Expression::CppUnwind(inner) => {
            let outcome = model(inner, effects);
            effects.destructors += 1;
            outcome
        }
    }
}

/// The R call that evaluates the expression.
fn build(r: &R, expression: &Expression) -> Sexp {
    let wrap = |routine: &str, inner: &Expression, with_env: bool| {
        let quoted = r.quote(build(r, inner));
        let mut arguments = vec![(None, r.string(routine)), (None, quoted)];
        if with_env {
            arguments.push((None, r.data("R_GlobalEnv")));
        }
        r.call_expression(".Call", &arguments)
    };
    match expression {
        Expression::Value(value) => r.call_expression("identity", &[(None, r.integer(*value))]),
        Expression::Error(message) => r.call_expression(
            ".Call",
            &[(None, r.string("rytest_error")), (None, r.string(message))],
        ),
        Expression::Eval(inner) => wrap("rytest_eval", inner, true),
        Expression::TryEval(inner) => wrap("rytest_try_eval", inner, false),
        Expression::Toplevel(inner) => wrap("rytest_toplevel", inner, false),
        Expression::TryCatch(inner) => wrap("rytest_try_catch", inner, false),
        Expression::UnwindProtect(inner) => wrap("rytest_unwind_protect", inner, false),
        Expression::ExecWithCleanup(inner) => wrap("rytest_exec_with_cleanup", inner, false),
        Expression::CppUnwind(inner) => wrap("rytest_cpp_unwind", inner, false),
    }
}

fn observe(r: &R, value: Result<Sexp, String>) -> Outcome {
    match value {
        Err(message) => Outcome::Error(
            message
                .strip_prefix("Error: ")
                .unwrap_or(&message)
                .trim_end_matches('\n')
                .to_owned(),
        ),
        Ok(value) => match r.type_of(value) {
            INTSXP => Outcome::Integer(r.integers(value)[0]),
            LGLSXP => Outcome::Logical(r.integers(value)[0] != 0),
            STRSXP => Outcome::Text(r.strings(value)[0].clone().unwrap_or_default()),
            kind => panic!("an unexpected result of type {kind}"),
        },
    }
}

fn counters(r: &R) -> Effects {
    let cleanups = r.integers(r.call("rytest_cleanup_state", &[]).expect("cleanup state"))[0];
    let destructors = r.integers(r.call("rytest_destructor_count", &[]).expect("destructors"))[0];
    Effects {
        cleanups,
        destructors,
    }
}

#[test]
fn fuzz_nested_non_local_exits() {
    let r = r();
    rytest(&r);
    let contexts = unsafe { (r.api().ry_context_depth)() };
    let arena = unsafe { (r.api().ry_arena_bytes)() };
    let mut rng = SplitMix64(0x5eed);
    for iteration in 0..fuzz_iterations(150) {
        let mark = r.protected.get();
        r.stress(iteration % 10 == 9);
        let expression = generate(&mut rng, 5);
        let mut expected_effects = Effects::default();
        let expected = model(&expression, &mut expected_effects);
        let depth = r.runtime_protect_depth();
        let before = counters(&r);
        let call = build(&r, &expression);
        let outcome = observe(&r, r.eval_expression(call));
        r.stress(false);
        let after = counters(&r);
        let effects = Effects {
            cleanups: after.cleanups - before.cleanups,
            destructors: after.destructors - before.destructors,
        };
        assert_eq!(outcome, expected, "{expression:?}");
        assert_eq!(effects, expected_effects, "{expression:?}");
        assert_eq!(
            unsafe { (r.api().ry_context_depth)() },
            contexts,
            "{expression:?} left a C context"
        );
        assert_eq!(
            r.runtime_protect_depth(),
            depth,
            "{expression:?} left the PROTECT stack unbalanced"
        );
        assert_eq!(
            unsafe { (r.api().ry_arena_bytes)() },
            arena,
            "{expression:?} kept R_alloc memory"
        );
        r.release_to(mark);
    }
}

#[derive(Clone, Debug)]
enum Stored {
    Integers(Vec<c_int>),
    Text(Vec<u8>),
}

#[test]
fn fuzz_api_sequences_keep_objects_intact() {
    let r = r();
    rytest(&r);
    let names = r.data::<Sexp>("R_NamesSymbol");
    for seed in 0..fuzz_iterations(12) as u64 {
        let mut rng = SplitMix64(seed);
        let mark = r.protected.get();
        r.stress(seed % 3 == 2);
        const SLOTS: usize = 16;
        let store = r.keep(unsafe { (r.api().Rf_allocVector)(VECSXP, SLOTS as isize) });
        let mut model: Vec<Option<Stored>> = vec![None; SLOTS];
        for _ in 0..120 {
            let slot = rng.below(SLOTS);
            match rng.below(6) {
                0 | 1 => {
                    let length = rng.below(40);
                    let values: Vec<c_int> = (0..length)
                        .map(|_| (rng.next_u64() as c_int).max(i32::MIN + 1))
                        .collect();
                    let vector = unsafe { (r.api().Rf_allocVector)(INTSXP, length as isize) };
                    unsafe {
                        std::slice::from_raw_parts_mut((r.api().INTEGER)(vector), length)
                            .copy_from_slice(&values);
                        (r.api().SET_VECTOR_ELT)(store, slot as isize, vector);
                    }
                    model[slot] = Some(Stored::Integers(values));
                }
                2 => {
                    let bytes: Vec<u8> = (0..rng.below(8))
                        .map(|_| b"abxy\xc3\xa9 "[rng.below(7)])
                        .collect();
                    let text = std::ffi::CString::new(bytes.clone()).expect("no NUL");
                    let vector = unsafe { (r.api().Rf_mkString)(text.as_ptr()) };
                    unsafe { (r.api().SET_VECTOR_ELT)(store, slot as isize, vector) };
                    model[slot] = Some(Stored::Text(bytes));
                }
                3 => {
                    // Character and back: integers survive the round trip exactly.
                    if let Some(Stored::Integers(values)) = &model[slot] {
                        let vector = r.element(store, slot);
                        let text =
                            r.keep(unsafe { (r.api().Rf_coerceVector)(vector, STRSXP as u32) });
                        let back = unsafe { (r.api().Rf_coerceVector)(text, INTSXP as u32) };
                        assert_eq!(&r.integers(back), values);
                        let copy = unsafe { (r.api().Rf_duplicate)(vector) };
                        assert_eq!(
                            unsafe { (r.api().R_compute_identical)(vector, copy, 16) },
                            1
                        );
                    }
                }
                4 => {
                    if let Some(Stored::Integers(values)) = &model[slot] {
                        let vector = r.element(store, slot);
                        let labels =
                            r.keep(unsafe { (r.api().Rf_coerceVector)(vector, STRSXP as u32) });
                        unsafe { (r.api().Rf_setAttrib)(vector, names, labels) };
                        let read = unsafe { (r.api().Rf_getAttrib)(vector, names) };
                        let expected: Vec<Option<String>> =
                            values.iter().map(|value| Some(value.to_string())).collect();
                        assert_eq!(r.strings(read), expected);
                    }
                }
                _ => unsafe { (r.api().ry_collect)((rng.below(3) == 0) as c_int) },
            }
            for (index, stored) in model.iter().enumerate() {
                let element = r.element(store, index);
                match stored {
                    None => assert_eq!(element, r.nil()),
                    Some(Stored::Integers(values)) => assert_eq!(&r.integers(element), values),
                    Some(Stored::Text(bytes)) => {
                        assert_eq!(
                            r.strings(element),
                            vec![Some(String::from_utf8_lossy(bytes).into_owned())]
                        );
                    }
                }
            }
        }
        r.stress(false);
        r.release_to(mark);
    }
}

/// A random object for the accessor comparison: every vector kind, ALTREP sequences, nested
/// lists, pairlists and calls with tags, and NULL.
fn random_object(r: &R, rng: &mut SplitMix64, depth: usize) -> Sexp {
    let length = rng.below(12);
    let choice = if depth == 0 { rng.below(7) } else { rng.below(10) };
    match choice {
        0 => r.nil(),
        1..=4 => {
            let kind = [LGLSXP, INTSXP, REALSXP, RAWSXP][choice - 1];
            let vector = r.keep(unsafe { (r.api().Rf_allocVector)(kind, length as isize) });
            unsafe {
                match kind {
                    REALSXP => std::slice::from_raw_parts_mut((r.api().REAL)(vector), length)
                        .iter_mut()
                        .for_each(|value| *value = f64::from_bits(rng.next_u64())),
                    RAWSXP => std::slice::from_raw_parts_mut((r.api().RAW)(vector), length)
                        .iter_mut()
                        .for_each(|value| *value = rng.next_u64() as u8),
                    _ => std::slice::from_raw_parts_mut((r.api().INTEGER)(vector), length)
                        .iter_mut()
                        .for_each(|value| *value = rng.next_u64() as c_int),
                }
            }
            vector
        }
        5 => {
            let vector = r.keep(unsafe { (r.api().Rf_allocVector)(STRSXP, length as isize) });
            for index in 0..length {
                let element = if rng.chance(1, 5) {
                    r.data("R_NaString")
                } else {
                    let bytes: Vec<u8> = (0..rng.below(6))
                        .map(|_| b"abz\xc3\xa9 "[rng.below(6)])
                        .collect();
                    unsafe {
                        (r.api().Rf_mkCharLenCE)(bytes.as_ptr().cast(), bytes.len() as c_int, 1)
                    }
                };
                unsafe { (r.api().SET_STRING_ELT)(vector, index as isize, element) };
            }
            vector
        }
        6 => r.keep(
            r.call(
                "rytest_sequence",
                &[r.integer(rng.below(100) as c_int), r.integer(length as c_int)],
            )
            .expect("an ALTREP sequence"),
        ),
        7 => {
            let list = r.keep(unsafe { (r.api().Rf_allocVector)(VECSXP, length as isize) });
            for index in 0..length {
                let element = random_object(r, rng, depth - 1);
                unsafe { (r.api().SET_VECTOR_ELT)(list, index as isize, element) };
            }
            list
        }
        _ => {
            let mut cells = r.nil();
            for position in (0..length.max(1)).rev() {
                let value = random_object(r, rng, depth - 1);
                cells = r.keep(unsafe {
                    if position == 0 && choice == 9 {
                        (r.api().Rf_lcons)(r.symbol("f"), cells)
                    } else {
                        (r.api().Rf_cons)(value, cells)
                    }
                });
                if rng.chance(1, 2) {
                    unsafe { (r.api().SET_TAG)(cells, r.symbol(&format!("t{position}"))) };
                }
            }
            cells
        }
    }
}

/// The inline accessors in `Rinternals.h` read the object header directly, so they must agree
/// with the libR functions on every object, including the ones they hand back to libR.
#[test]
fn fuzz_inline_accessors_agree_with_libr() {
    let r = r();
    rytest(&r);
    let mut rng = SplitMix64(0xacce55);
    for iteration in 0..fuzz_iterations(400) {
        let mark = r.protected.get();
        let object = random_object(&r, &mut rng, 3);
        let disagreement = r
            .call("rytest_accessor_agreement", &[r.quote(object)])
            .expect("the comparison runs");
        assert_eq!(
            r.strings(disagreement),
            vec![Some(String::new())],
            "iteration {iteration}: the inline accessor disagrees with libR"
        );
        r.release_to(mark);
    }
}
