//! The interpreter's fuzz contracts, shared by the in-tree harness (`tests/test_fuzz.rs`) and the
//! coverage-guided `fuzz/` targets so both check exactly the same properties. Each one runs
//! without base R or libR, on an interpreter it installs on first use.

use {
    crate::{
        bytecode, code_image, deparse, eval,
        graphics_render::{
            Affine, Clip, Definition, Drawing, Extend, Fill, FillRule, Format, Item, Mark,
            Operator, Outline, Page, Pattern, Rect, Segment, Stop, Stroke, circle_outline,
            render_files,
        },
        lower, serialize,
        unwind::Unwind,
        values,
    },
    heap::{
        Heap,
        object::{Sexp, SexpType},
    },
    std::sync::{Mutex, Once},
    syntax::testing::Entropy,
};

/// Runs `body` with the interpreter installed. The interpreter is single-threaded, so callers on
/// different threads take turns.
pub fn with_interpreter<T>(body: impl FnOnce() -> T) -> T {
    static INSTALL: Once = Once::new();
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    INSTALL.call_once(|| {
        let heap: &'static mut Heap = Box::leak(Box::new(Heap::new()));
        // SAFETY: the heap lives for the rest of the process, and the lock above keeps every
        // use of the interpreter on one thread at a time.
        unsafe {
            crate::install(heap, crate::default_hooks());
            crate::interp().show_error_messages = false;
        }
    });
    body()
}

/// Lowering any text must not panic: it either fails with a parse error or produces expressions,
/// and every expression it produces must deparse without panicking.
pub fn check_lowering_input(text: &str) {
    with_interpreter(|| {
        let mark = crate::heap().handle_mark();
        // SAFETY: the interpreter is installed.
        unsafe {
            if let Ok(expressions) = lower::parse_text(text, "<fuzz>") {
                for index in 0..values::length(expressions) {
                    deparse::deparse_lines(
                        expressions.element(index),
                        60,
                        deparse::options::KEEP_INTEGER,
                    );
                }
            }
        }
        crate::heap().release_handles(mark);
    });
}

/// Reading any byte stream as a serialized object must fail with an R error or succeed, never
/// panic.
pub fn check_unserialize_input(bytes: &[u8]) {
    with_interpreter(|| {
        let mark = crate::heap().handle_mark();
        // SAFETY: the interpreter is installed.
        unsafe {
            drop(serialize::unserialize(bytes, values::nil()));
        }
        crate::truncate_frames(crate::interp(), 0);
        crate::interp().foreign_calls.clear();
        crate::heap().release_handles(mark);
    });
}

/// Generates a program over `a` and `b` and checks that the tier-0 bytecode agrees with the AST
/// evaluator on it: the program's value evaluated directly must equal its value as a closure body
/// on each of the first three calls, the second and later of which run compiled bytecode. The
/// compiled body runs in a slot frame, so it also runs once more with the frame given its
/// environment before the body and once after it, with invariants on, so each reification is
/// checked against the bindings the heap-frame path would have made.
pub fn check_bytecode_agreement(entropy: &mut impl Entropy) {
    syntax::testing::enable_invariants();
    let body = program(entropy);
    let arguments = [
        "3L",
        "-2.5",
        "NA",
        "TRUE",
        "2147483647L",
        "0",
        "NA_integer_",
        "[3L]",
        "[-2.5, 1]",
        "[TRUE]",
    ];
    let a = arguments[entropy.below(arguments.len())];
    let b = arguments[entropy.below(arguments.len())];
    with_interpreter(|| {
        // SAFETY: the interpreter is installed.
        let (direct, compiled) = unsafe { outcomes(&body, a, b) };
        for (call, outcome) in compiled.iter().enumerate() {
            assert_eq!(
                &direct,
                outcome,
                "call {}: `{body}` with a = {a}, b = {b}",
                call + 1
            );
        }
    });
}

/// One compiled arithmetic site, called with a seeded sequence of operand pairs of changing
/// kinds, agrees with the AST evaluator on every call. Tier 0 specializes the site to the kinds
/// it sees and must undo the specialization when they change, give up after repeated undos,
/// and leave an NA or overflowing integer result and every operand that is not a plain scalar
/// to the generic instruction.
pub fn check_quickening_agreement(entropy: &mut impl Entropy) {
    syntax::testing::enable_invariants();
    const OPERANDS: &[&str] = &[
        "3L",
        "-7L",
        "2147483647L",
        "-2147483647L",
        "NA_integer_",
        "0L",
        "2.5",
        "-0.0",
        "NaN",
        "NA_real_",
        "Inf",
        "1e308",
        "TRUE",
        "NA",
        "c(1L, 2L)",
        "c(a = 1)",
        "c(x = 2L)",
        "[3L]",
        "[1.5]",
    ];
    const OPERATORS: &[&str] = &[
        "+", "-", "*", "/", "==", "!=", "<", ">", "<=", ">=", "^", "%%",
    ];
    let operator = OPERATORS[entropy.below(OPERATORS.len())];
    let body = match entropy.below(3) {
        0 => format!("a {operator} b"),
        1 => format!("r <- a; j <- 0L; while (j < 3L) {{ r <- r {operator} b; j <- j + 1L }}; r"),
        _ => format!("x <- a {operator} b; y <- b {operator} a; c(x, y)"),
    };
    let mut calls: Vec<(&str, &str)> = Vec::new();
    for _ in 0..2 + entropy.below(10) {
        // Runs of one kind let a site specialize before the kind changes.
        let a = OPERANDS[entropy.below(OPERANDS.len())];
        let b = OPERANDS[entropy.below(OPERANDS.len())];
        for _ in 0..1 + entropy.below(3) {
            calls.push((a, b));
        }
    }
    with_interpreter(|| {
        // SAFETY: the interpreter is installed.
        unsafe {
            run(&format!("f <- function(a, b) {{ {body} }}"));
            for (call, &(a, b)) in calls.iter().enumerate() {
                let direct = run(&format!("a <- {a}\nb <- {b}\n{{ {body} }}"));
                let compiled = run(&format!("f({a}, {b})"));
                assert_eq!(
                    direct,
                    compiled,
                    "call {}: `{body}` with a = {a}, b = {b}, after {:?}",
                    call + 1,
                    &calls[..call]
                );
            }
        }
    });
}

/// Tier 0 resumes an activation at any pc it reaches, as it does when the optimizing tier
/// deoptimizes there. A test tier takes every activation of a random closure over at entry,
/// single-steps it through `k` instructions, and hands it back through the same
/// [`bytecode::TierExit::Resume`] path compiled code exits by. For every `k` up to the number
/// of instructions the call runs, the result must equal tier 0's alone.
pub fn check_resume_at_every_pc(entropy: &mut impl Entropy) {
    use {
        bytecode::{Machine, Stepped, Tier, TierExit},
        std::cell::Cell,
    };
    thread_local! {
        static TARGET: Cell<Option<Sexp>> = const { Cell::new(None) };
        static BUDGET: Cell<Option<usize>> = const { Cell::new(None) };
        static STEPS: Cell<usize> = const { Cell::new(0) };
    }
    unsafe fn enter(machine: &mut Machine<'_>) -> Option<TierExit> {
        if TARGET.get() != Some(machine.function) {
            return None;
        }
        // Only the outermost activation is taken over, so a recursive call runs in tier 0.
        let budget = BUDGET.take()?;
        let mut pc = 0;
        for taken in 0..budget {
            match unsafe { bytecode::step(machine, pc) } {
                Ok(Stepped::At(next)) => pc = next,
                Ok(Stepped::Returned(value)) => {
                    STEPS.set(taken + 1);
                    return Some(TierExit::Finished(Ok(value)));
                }
                Err(unwind) => {
                    STEPS.set(taken + 1);
                    return Some(TierExit::Finished(Err(unwind)));
                }
            }
        }
        STEPS.set(budget);
        Some(TierExit::Resume { pc, pending: None })
    }
    syntax::testing::enable_invariants();
    let body = program(entropy);
    let arguments = [
        "3L",
        "-2.5",
        "NA",
        "TRUE",
        "2147483647L",
        "0",
        "[3L]",
        "[-2.5, 1]",
    ];
    let a = arguments[entropy.below(arguments.len())];
    let b = arguments[entropy.below(arguments.len())];
    with_interpreter(|| {
        // SAFETY: the interpreter is installed, and the test tier is removed before returning.
        unsafe {
            // Restores the tier even when an assertion fails, so later tests sharing the
            // interpreter do not run under the test tier.
            struct Restore(Option<Tier>);
            impl Drop for Restore {
                fn drop(&mut self) {
                    TARGET.set(None);
                    bytecode::set_tier(self.0);
                }
            }
            let _restore = Restore(bytecode::tier().copied());
            // Installed before `f` exists, so its first entry already schedules the tier.
            bytecode::set_tier(Some(Tier {
                enter,
                entry_threshold: 1,
                worth_recording: |_| false,
                back_edge: |_, _| None,
                back_edge_threshold: bytecode::NEVER,
                baseline: |_| {},
                baseline_threshold: bytecode::NEVER,
                call: |_, _| None,
            }));
            let reset = format!("a <- {a}\nb <- {b}\ns <- 7L\nk <- 0L\ni <- 0L");
            run(&reset);
            run(&format!("f <- function(a, b) {{ {body} }}"));
            let function = values::install("f")
                .and_then(|symbol| eval::eval(symbol, values::global_env()))
                .ok();
            TARGET.set(function);
            let call = format!("f({a}, {b})");
            // With no budget the test tier declines, so this call runs in tier 0 alone.
            let alone = run(&call);
            let mut budget = 0;
            loop {
                run(&reset);
                BUDGET.set(Some(budget));
                STEPS.set(0);
                let resumed = run(&call);
                assert_eq!(
                    alone, resumed,
                    "resumed after {budget} instructions: `{body}` with a = {a}, b = {b}"
                );
                assert!(BUDGET.take().is_none(), "the test tier never took `f` over");
                if STEPS.get() < budget {
                    break;
                }
                budget += 1;
            }
        }
    });
}

/// A random element-wise tree over vector operands, the shape the tier-0 compiler fuses into
/// one pass, gives the same value from the fused pass as from the AST evaluator: the same NA
/// and NaN payloads, the same NA on integer overflow, the same recycling and reductions.
pub fn check_fused_agreement(entropy: &mut impl Entropy) {
    let operands = [
        "rep(c(1.5, NA, -2, NaN, 0, 1e308), 40)",
        "rep(c(NaN, 2, NA, -0.5, 3, -1e308), 40)",
        "rep(c(2147483647L, -3L, NA, 0L, 7L, -2147483647L), 40)",
        "rep(c(1L, 2L, 3L, 4L, 5L, 6L), 40)",
        "rep(c(TRUE, NA, FALSE, TRUE, FALSE, TRUE), 40)",
        "rep(c(0.25, -4, 9, 1, 16, 100), 40)",
        "rep(c(2, -1), 40)",
        "rep(c(3L, 1L, 2L), 40)",
        "c(2, -1)",
        "5",
        "-2L",
        // Builtins only: the interpreter here has no base R closures such as `complex()`.
        "rep(c(1.5-2i, NA, NaN+1i, Inf+0i, 0i, -1e308+1e308i), 40)",
        "rep(c(0.5+0.5i, -1i, 2+0i), 80)",
        "2+0i",
    ];
    let a = operands[entropy.below(operands.len())];
    let b = operands[entropy.below(operands.len())];
    let tree = fused_expression(entropy, 3);
    let body = match entropy.below(5) {
        0 => format!("sum({tree})"),
        1 => format!("any({tree})"),
        2 => format!("all({tree})"),
        _ => tree,
    };
    with_interpreter(|| {
        // SAFETY: the interpreter is installed.
        let (direct, compiled) = unsafe { outcomes(&body, a, b) };
        for (call, outcome) in compiled.iter().enumerate() {
            assert_eq!(
                &direct,
                outcome,
                "call {}: `{body}` with a = {a}, b = {b}",
                call + 1
            );
        }
    });
}

/// Generates a program that reads and writes attributes, names and elements and combines values
/// with `c()`, and checks that the compiled closure, whose calls take the builtins' direct
/// entries, agrees with the AST evaluator, whose calls take the full builtins.
pub fn check_element_access_agreement(entropy: &mut impl Entropy) {
    let body = element_program(entropy);
    let a = ELEMENT_ARGUMENTS[entropy.below(ELEMENT_ARGUMENTS.len())];
    let b = ELEMENT_INDICES[entropy.below(ELEMENT_INDICES.len())];
    with_interpreter(|| {
        // SAFETY: the interpreter is installed.
        let (direct, compiled) = unsafe { outcomes(&body, a, b) };
        for (call, outcome) in compiled.iter().enumerate() {
            assert_eq!(
                &direct,
                outcome,
                "call {}: `{body}` with a = {a}, b = {b}",
                call + 1
            );
        }
    });
}

fn fused_expression(entropy: &mut impl Entropy, depth: usize) -> String {
    if depth == 0 || entropy.below(4) == 0 {
        let leaves = ["a", "b", "a", "b", "2", "1L", "0.5", "NA", "TRUE"];
        // Calls the pass cannot compute, which it evaluates ahead into registers: some with
        // side effects whose order against the variable reads shows in the value.
        let calls = [
            "rev(a)",
            "mean(b)",
            "{ k <<- k + 1L; k }",
            "{ assign(\"a\", rev(a)); 2 }",
            "`-`(b)",
        ];
        if entropy.below(5) == 0 {
            return calls[entropy.below(calls.len())].to_owned();
        }
        return leaves[entropy.below(leaves.len())].to_owned();
    }
    match entropy.below(8) {
        0 => format!("-({})", fused_expression(entropy, depth - 1)),
        1 => format!("!({})", fused_expression(entropy, depth - 1)),
        2 => {
            let functions = [
                "exp", "sqrt", "abs", "floor", "log10", "sin", "trunc", "sign",
            ];
            format!(
                "{}({})",
                functions[entropy.below(functions.len())],
                fused_expression(entropy, depth - 1)
            )
        }
        3 => format!(
            "({} {} {})",
            fused_expression(entropy, depth - 1),
            ["&", "|"][entropy.below(2)],
            fused_expression(entropy, depth - 1)
        ),
        _ => {
            let operators = ["+", "-", "*", "/", "^", "==", "<", ">=", "!="];
            format!(
                "({} {} {})",
                fused_expression(entropy, depth - 1),
                operators[entropy.below(operators.len())],
                fused_expression(entropy, depth - 1)
            )
        }
    }
}

unsafe fn outcomes(body: &str, a: &str, b: &str) -> (String, Vec<String>) {
    unsafe {
        // The direct run assigns its locals globally, which the closure would then read, so
        // every run starts from the same globals.
        let reset = format!("a <- {a}\nb <- {b}\ns <- 7L\nk <- 0L\ni <- 0L");
        // A reset that errors part way would leave the previous case's globals behind.
        assert_ne!(
            run(&reset),
            "error",
            "the operands do not evaluate: {reset}"
        );
        let direct = run(&format!("local_value <- {{ {body} }}\nlocal_value"));
        run(&format!("f <- function(a, b) {{ {body} }}"));
        // `caller()` hands out its caller's environment, which makes a slot frame reify.
        run("parent <- function(n) .Internal(parent.frame(n))");
        run("caller <- function() parent(1L)");
        run(&format!("before <- function(a, b) {{ caller(); {body} }}"));
        run(&format!(
            "after <- function(a, b) {{ result <- {{ {body} }}; caller(); result }}"
        ));
        let mut compiled: Vec<String> = (0..3)
            .map(|_| {
                run(&reset);
                run(&format!("f({a}, {b})"))
            })
            .collect();
        for function in ["before", "after"] {
            for _ in 0..2 {
                run(&reset);
                compiled.push(run(&format!("{function}({a}, {b})")));
            }
        }
        (direct, compiled)
    }
}

/// `Sys.getenv`, `Sys.setenv` and `Sys.unsetenv` agree with the C library, compared by their
/// deparsed results because the bare interpreter has no `identical()`: after a random
/// sequence of changes made from R and from Rust, every name reads back what `getenv()` returns,
/// a set reports which names `setenv()` accepts, and an unset reports `TRUE` for every name. The
/// names overlap as prefixes of one another, repeat within a call, are empty, hold `=` or are not
/// ASCII, and a value may itself look like the rest of an entry (`A` set to `B=x` is what a lookup
/// of `A=B` finds).
pub fn check_environment_agreement(entropy: &mut impl Entropy) {
    const NAMES: &[&str] = &[
        "RY_FUZZ_ENV",
        "RY_FUZZ_ENV_A",
        "RY_FUZZ_ENVA",
        "RY_FUZZ_EN",
        "RY_FUZZ_ENV=B",
        "",
        "RY_FUZZ_\u{e9}",
        "NA",
    ];
    const VALUES: &[&str] = &["a", "", "B=x", "\u{e9}t\u{e9}", "NA", "x y", "=", "long"];
    fn text(entropy: &mut impl Entropy, table: &[&str]) -> String {
        match table[entropy.below(table.len())] {
            "long" => "v".repeat(300),
            other => other.to_owned(),
        }
    }
    fn quoted(text: &str) -> String {
        let mut out = String::from("\"");
        for character in text.chars() {
            match character {
                '"' | '\\' => {
                    out.push('\\');
                    out.push(character);
                }
                ' '..='~' => out.push(character),
                _ => out.push_str(&format!("\\u{{{:x}}}", u32::from(character))),
            }
        }
        out.push('"');
        out
    }
    fn vector(items: &[String]) -> String {
        let items: Vec<String> = items.iter().map(|item| quoted(item)).collect();
        format!("c({})", items.join(", "))
    }
    fn valid(name: &str) -> bool {
        !name.is_empty() && !name.contains('=')
    }
    with_interpreter(|| {
        for name in NAMES.iter().filter(|name| valid(name)) {
            // SAFETY: the interpreter lock is held, and nothing else in the process changes these
            // variables.
            unsafe { std::env::remove_var(name) };
        }
        for step in 0..12 {
            let count = 1 + entropy.below(3);
            let names: Vec<String> = (0..count).map(|_| text(entropy, NAMES)).collect();
            match entropy.below(4) {
                0 => {
                    for name in names.iter().filter(|name| valid(name)) {
                        let value = text(entropy, VALUES);
                        // SAFETY: as above.
                        unsafe { std::env::set_var(name, value) };
                    }
                }
                1 => {
                    let new_values: Vec<String> =
                        (0..count).map(|_| text(entropy, VALUES)).collect();
                    let accepted: Vec<&str> = names
                        .iter()
                        .map(|name| if valid(name) { "TRUE" } else { "FALSE" })
                        .collect();
                    let call = format!(
                        ".Internal(Sys.setenv({}, {}))",
                        vector(&names),
                        vector(&new_values)
                    );
                    let expected = format!("c({})", accepted.join(", "));
                    // SAFETY: the interpreter is installed.
                    let (actual, expected) = unsafe { (run(&call), run(&expected)) };
                    assert_eq!(actual, expected, "step {step}: {call}");
                    // The last of a repeated name wins, as consecutive `setenv()` calls leave it.
                    for (name, value) in names.iter().zip(&new_values).rev() {
                        if valid(name) {
                            assert_eq!(
                                std::env::var(name).ok().as_deref(),
                                Some(value.as_str()),
                                "step {step}: {call} left {name}"
                            );
                            break;
                        }
                    }
                }
                2 => {
                    let call = format!(".Internal(Sys.unsetenv({}))", vector(&names));
                    let expected = format!("c({})", vec!["TRUE"; count].join(", "));
                    // SAFETY: the interpreter is installed.
                    let (actual, expected) = unsafe { (run(&call), run(&expected)) };
                    assert_eq!(actual, expected, "step {step}: {call}");
                    for name in names.iter().filter(|name| valid(name)) {
                        assert!(
                            std::env::var_os(name).is_none(),
                            "step {step}: {call} kept {name}"
                        );
                    }
                }
                _ => {}
            }
            let probe: Vec<String> = (0..1 + entropy.below(4))
                .map(|_| text(entropy, NAMES))
                .collect();
            let expected: Vec<String> = probe
                .iter()
                .map(|name| {
                    let found = (!name.is_empty()).then(|| std::env::var_os(name)).flatten();
                    found.map_or_else(
                        || "<unset>".to_owned(),
                        |value| value.to_string_lossy().into_owned(),
                    )
                })
                .collect();
            let call = format!(".Internal(Sys.getenv({}, \"<unset>\"))", vector(&probe));
            // SAFETY: the interpreter is installed.
            let (actual, expected) = unsafe { (run(&call), run(&vector(&expected))) };
            assert_ne!(actual, "error", "step {step}: {call}");
            assert_eq!(actual, expected, "step {step}: {call}");
        }
        for name in NAMES.iter().filter(|name| valid(name)) {
            // SAFETY: as above.
            unsafe { std::env::remove_var(name) };
        }
    });
}

/// Bodies that put into a code object what the arithmetic generator does not: strings, nested
/// functions, named arguments and calls the compiler leaves to the evaluator.
const IMAGE_BODIES: &[&str] = &[
    "g <- function(x, y = 2L) x * y + a; g(b) + g(a, y = 3L)",
    "v <- c(first = a, second = b); if (length(v) > 1L) v[[\"second\"]] else \"none\"",
    "h <- function(n) { total <- 0; for (i in seq_len(n)) total <- total + i; total }; h(3L) + a",
    "s <- list(x = a, y = \"text\", z = NULL); is.null(s$z) && !is.null(s$x)",
    "k <- 0L; repeat { k <- k + 1L; if (k >= 3L) break }; k * b",
    // Calling `g` compiles its argument `one()` into promise code, which has no statement record
    // and no argument after the call, so the call site's three constants end that code's table.
    "one <- function() a; g <- function(x) x; g(one())",
];

/// A compiled closure survives the code image's encoding: decoding its code against the same
/// body gives an object that encodes to the same bytes and computes what the AST evaluator
/// computes, trained or not, and every mutation of the encoding decodes or fails with an R error.
pub fn check_code_image_round_trip(entropy: &mut impl Entropy) {
    let body = if entropy.below(3) == 0 {
        IMAGE_BODIES[entropy.below(IMAGE_BODIES.len())].to_owned()
    } else {
        program(entropy)
    };
    let arguments = ["3L", "-2.5", "NA", "TRUE", "0", "[3L]"];
    let a = arguments[entropy.below(arguments.len())];
    let b = arguments[entropy.below(arguments.len())];
    let mutations: Vec<(usize, u8)> = (0..8)
        .map(|_| (entropy.below(1 << 16), entropy.below(256) as u8))
        .collect();
    code_image_round_trip(&body, a, b, &mutations);
}

/// Every body of the image battery survives the round trip of
/// [`check_code_image_round_trip`], so a shape the battery pins is checked on every run rather
/// than only on the seeds that draw it.
pub fn check_image_bodies_round_trip() {
    for body in IMAGE_BODIES {
        code_image_round_trip(body, "3L", "-2.5", &[]);
    }
}

fn code_image_round_trip(body: &str, a: &str, b: &str, mutations: &[(usize, u8)]) {
    with_interpreter(|| {
        // SAFETY: the interpreter is installed, and every object it reads is rooted by the
        // global environment or the handle scope.
        unsafe {
            let (direct, _) = outcomes(&body, a, b);
            let mark = crate::heap().handle_mark();
            let reset = format!("a <- {a}\nb <- {b}\ns <- 7L\nk <- 0L\ni <- 0L");
            let global = values::global_env();
            let compiled = (|| -> Result<Option<(Sexp, Sexp, Sexp)>, Unwind> {
                let function = eval::find_var(values::install("f")?, global)?;
                let code = bytecode::compiled_body(function)?;
                if code.kind() != SexpType::Bytecode {
                    return Ok(None);
                }
                Ok(Some((function, code, bytecode::source_body(function))))
            })();
            let Ok(Some((function, code, source))) = compiled else {
                crate::heap().release_handles(mark);
                return;
            };
            for trained in [false, true] {
                let Some(bytes) = code_image::encode(code, source, trained) else {
                    continue;
                };
                let decoded =
                    code_image::decode_untrusted(&bytes, source).unwrap_or_else(|failure| {
                        panic!("`{body}`: an encoded code object must decode: {failure:?}")
                    });
                crate::heap().root(decoded);
                assert_eq!(
                    code_image::encode(decoded, source, trained).as_deref(),
                    Some(&bytes[..]),
                    "`{body}`: decoding then encoding must give the same bytes (trained: {trained})"
                );
                let copy = eval::make_closure(function.car(), source, values::nil(), global)
                    .unwrap_or_else(|_| panic!("cannot make a closure"));
                crate::heap().set_cdr(copy, decoded);
                eval::define(
                    values::install("g_image").unwrap_or_else(|_| panic!("cannot install")),
                    copy,
                    global,
                )
                .unwrap_or_else(|_| panic!("cannot define the decoded closure"));
                run(&reset);
                let outcome = run(&format!("g_image({a}, {b})"));
                assert_eq!(
                    direct, outcome,
                    "`{body}` with a = {a}, b = {b}: the decoded code must compute what the \
                     evaluator does (trained: {trained})"
                );
                for &(position, value) in mutations {
                    let mut mutated = bytes.clone();
                    let at = position % mutated.len().max(1);
                    match mutated.get_mut(at) {
                        Some(byte) if value % 4 != 0 => *byte ^= value,
                        _ => mutated.truncate(at),
                    }
                    // A mutation may decode to some other valid object; it must only never panic.
                    if let Ok(object) = code_image::decode_untrusted(&mutated, source) {
                        crate::heap().root(object);
                    }
                }
            }
            crate::heap().release_handles(mark);
        }
    });
}

/// An image file's record table and streams read back as they were written, an image of another
/// build or another version of its database reads as absent, and a corrupted file reads as absent
/// or fails with a message, never panics or names a payload or stream outside the file.
pub fn check_code_image_file(entropy: &mut impl Entropy) {
    let count = entropy.below(6);
    let mut offset = 0u64;
    let mut records = Vec::new();
    let mut streams = Vec::new();
    let mut payloads = Vec::new();
    for _ in 0..count {
        offset += 1 + entropy.below(1000) as u64;
        records.push((offset, entropy.below(5000) as u32));
        streams.push(
            (0..entropy.below(60))
                .map(|_| entropy.below(256) as u8)
                .collect::<Vec<u8>>(),
        );
        payloads.push(
            (0..entropy.below(40))
                .map(|_| entropy.below(256) as u8)
                .collect::<Vec<u8>>(),
        );
    }
    let stamp = code_image::DatabaseStamp {
        length: offset + entropy.below(100) as u64,
        modified: entropy.next_u64(),
    };
    let table: Vec<(u64, u32, &[u8])> = records
        .iter()
        .zip(&streams)
        .map(|(&(offset, length), stream)| (offset, length, &stream[..]))
        .collect();
    let build = "fuzz-build";
    let trained = entropy.below(2) == 0;
    let mutations: Vec<(usize, u8)> = (0..1 + entropy.below(4))
        .map(|_| (entropy.below(1 << 16), entropy.below(256) as u8))
        .collect();
    with_interpreter(|| {
        let bytes = code_image::image_bytes(build, trained, stamp, &table, &payloads, None)
            .unwrap_or_else(|_| panic!("a small image must write"));
        let entries = code_image::parse(&bytes, build, stamp)
            .unwrap_or_else(|message| panic!("a written image must parse: {message}"))
            .unwrap_or_else(|| panic!("a written image must belong to its build"))
            .entries;
        assert_eq!(entries.len(), records.len());
        let at = |offset: u64, length: u32| {
            let start = offset as usize;
            bytes.get(start..start + length as usize)
        };
        for (entry, (record, (stream, payload))) in entries
            .iter()
            .zip(records.iter().zip(streams.iter().zip(&payloads)))
        {
            assert_eq!((entry.offset, entry.length), *record);
            assert_eq!(
                at(entry.payload_offset, entry.payload_length),
                Some(&payload[..])
            );
            assert_eq!(
                at(entry.stream_offset, entry.stream_length),
                Some(&stream[..])
            );
        }
        assert_eq!(code_image::parse(&bytes, "another-build", stamp), Ok(None));
        for other in [
            code_image::DatabaseStamp {
                length: stamp.length + 1,
                ..stamp
            },
            code_image::DatabaseStamp {
                modified: stamp.modified ^ 1,
                ..stamp
            },
        ] {
            assert_eq!(code_image::parse(&bytes, build, other), Ok(None));
        }
        let mut mutated = bytes.clone();
        for &(position, value) in &mutations {
            let at = position % mutated.len().max(1);
            match mutated.get_mut(at) {
                Some(byte) if value % 4 != 0 => *byte ^= value,
                _ => mutated.truncate(at),
            }
        }
        if let Ok(Some(parsed)) = code_image::parse(&mutated, build, stamp) {
            assert!(
                parsed.native_offset + parsed.native_length <= mutated.len() as u64,
                "a parsed image must not name a native section outside the file"
            );
            for entry in parsed.entries {
                assert!(
                    entry.payload_offset + u64::from(entry.payload_length) <= mutated.len() as u64,
                    "a parsed image must not name a payload outside the file"
                );
                assert!(
                    entry.stream_offset + u64::from(entry.stream_length) <= mutated.len() as u64,
                    "a parsed image must not name a stream outside the file"
                );
            }
        }
    });
}

/// How [`check_code_image_load`] damages the image it loads from.
const LOAD_MODES: usize = 6;

/// A closure fetched from a lazy-load database through its image computes what the evaluator
/// computes, whatever the image holds: a damaged file, a record whose closure count or entry does
/// not match, or a closure that shares the fetched body under other formals all fall back to
/// compiling, and code the decoder accepts from a mutated but rehashed payload runs without
/// crashing.
pub fn check_code_image_load(entropy: &mut impl Entropy) {
    let body = if entropy.below(3) == 0 {
        IMAGE_BODIES[entropy.below(IMAGE_BODIES.len())].to_owned()
    } else {
        program(entropy)
    };
    let arguments = ["3L", "-2.5", "NA", "TRUE", "0", "[3L]"];
    let a = arguments[entropy.below(arguments.len())];
    let b = arguments[entropy.below(arguments.len())];
    let mode = entropy.below(LOAD_MODES);
    let trained = entropy.below(2) == 0;
    let mutations: Vec<(usize, u8)> = (0..1 + entropy.below(4))
        .map(|_| (entropy.below(1 << 16), entropy.below(256) as u8))
        .collect();
    let directory = std::env::temp_dir().join(format!(
        "ry-code-image-load-{}-{}",
        std::process::id(),
        entropy.next_u64()
    ));
    with_interpreter(|| {
        // SAFETY: the interpreter is installed, and every object it reads is rooted by the
        // global environment or the handle scope.
        unsafe {
            let (direct, _) = outcomes(&body, a, b);
            let mark = crate::heap().handle_mark();
            let global = values::global_env();
            let reset = format!("a <- {a}\nb <- {b}\ns <- 7L\nk <- 0L\ni <- 0L");
            let function = eval::find_var(
                values::install("f").unwrap_or_else(|_| panic!("cannot install")),
                global,
            )
            .unwrap_or_else(|_| panic!("`f` must be defined"));
            let mut stream = b"X\n".to_vec();
            for word in [2, 0x0004_0300, 0x0002_0300] {
                stream.extend_from_slice(&i32::to_be_bytes(word));
            }
            // A closure is its flags, its environment, its formals and its body.
            for word in [3 | 1 << 10, 253] {
                stream.extend_from_slice(&i32::to_be_bytes(word));
            }
            let source = bytecode::source_body(function);
            if !(xdr_item(&mut stream, function.car()) && xdr_item(&mut stream, source)) {
                crate::heap().release_handles(mark);
                return;
            }
            // The training run of an install: the record's closure runs before its code is
            // encoded, so a trained image carries the run's feedback.
            let (fetched, closures) = serialize::unserialize_with_closures(&stream, values::nil())
                .unwrap_or_else(|_| panic!("`{body}`: a written closure must read back"));
            crate::heap().root(fetched);
            let define = |name: &str, value: Sexp| {
                eval::define(
                    values::install(name).unwrap_or_else(|_| panic!("cannot install")),
                    value,
                    global,
                )
                .unwrap_or_else(|_| panic!("cannot define `{name}`"));
            };
            define("g_train", fetched);
            if trained {
                for _ in 0..3 {
                    run(&reset);
                    run(&format!("g_train({a}, {b})"));
                }
            }
            let pairs: Vec<(Sexp, Sexp)> = closures
                .iter()
                .map(|&closure| (closure, bytecode::source_body(closure)))
                .collect();
            let mut summary = code_image::Summary::default();
            let mut payload = code_image::record_payload(&pairs, trained, &mut summary);
            let mut offset = 0u64;
            match mode {
                // The record names more closures than the stream makes.
                1 => {
                    payload[0] = payload[0].wrapping_add(1);
                }
                // The record's one closure has no code in the image.
                2 => payload = vec![1, 0],
                // No entry for the record that is fetched.
                3 => offset = 7,
                // The payload is mutated and then rehashed, so only the decoder and the
                // verifier stand between it and the interpreter.
                4 => {
                    for &(position, value) in &mutations {
                        let at = 1 + position % payload.len().max(1);
                        if let Some(byte) = payload.get_mut(at) {
                            *byte ^= value | 1;
                        }
                    }
                }
                _ => {}
            }
            std::fs::create_dir_all(&directory).unwrap_or_else(|failure| {
                panic!("cannot create {}: {failure}", directory.display())
            });
            let database = directory.join("fuzz.rdb");
            let database = database
                .to_str()
                .unwrap_or_else(|| panic!("a temporary path is text"));
            std::fs::write(database, vec![0u8; stream.len()])
                .unwrap_or_else(|failure| panic!("cannot write {database}: {failure}"));
            let stamp = code_image::DatabaseStamp::of(database)
                .unwrap_or_else(|failure| panic!("{failure}"))
                .unwrap_or_else(|| panic!("a temporary file has a modification time"));
            let build = "fuzz-build";
            let mut image = code_image::image_bytes(
                build,
                trained,
                stamp,
                &[(offset, stream.len() as u32, &stream[..])],
                &[payload],
                None,
            )
            .unwrap_or_else(|_| panic!("a small image must write"));
            // Raw damage: the file no longer matches its own hashes or framing.
            if mode == 0 {
                for &(position, value) in &mutations {
                    let at = position % image.len().max(1);
                    match image.get_mut(at) {
                        Some(byte) if value % 4 != 0 => *byte ^= value,
                        _ => image.truncate(at),
                    }
                }
            }
            std::fs::write(code_image::path(database), &image)
                .unwrap_or_else(|failure| panic!("cannot write the image: {failure}"));
            let previous = std::mem::replace(&mut crate::interp().code_image.build, build);
            let show = std::mem::replace(&mut crate::interp().show_error_messages, false);
            let loaded = (|| -> Result<Option<Sexp>, Unwind> {
                // The database holds zeros, so a record the image does not hold is read from the
                // stream directly, as decompressing it from the database would give it.
                let number = code_image::database(values::mkchar(database)?);
                let value = match code_image::source(number, 0, stream.len())? {
                    code_image::Source::Image(record) => {
                        code_image::read_image_record(number, &record, values::nil())?
                    }
                    _ => serialize::unserialize(&stream, values::nil())?,
                };
                Ok(Some(value))
            })();
            if let Ok(Some(value)) = loaded {
                crate::heap().root(value);
                assert_eq!(
                    value.kind(),
                    SexpType::Closure,
                    "`{body}`: a record reads back"
                );
                define("g_image", value);
                // Another closure over the fetched body must not take the code compiled for the
                // fetched closure's formals.
                let template = lower::parse_text("function(a, b, extra) NULL", "<fuzz>")
                    .and_then(|expressions| {
                        crate::heap().root(expressions);
                        eval::eval(expressions.element(0), global)
                    })
                    .unwrap_or_else(|_| panic!("cannot make a function"));
                crate::heap().root(template);
                let formals = template.car();
                let other = eval::make_closure(formals, value.cdr(), values::nil(), global)
                    .unwrap_or_else(|_| panic!("cannot make a closure"));
                crate::heap().root(other);
                assert_eq!(
                    code_image::take_pending(other, value.cdr()).ok().flatten(),
                    None,
                    "`{body}`: a closure with other formals took the fetched closure's code"
                );
                for call in 0..3 {
                    run(&reset);
                    let outcome = run(&format!("g_image({a}, {b})"));
                    if mode != 4 {
                        assert_eq!(
                            direct,
                            outcome,
                            "`{body}` with a = {a}, b = {b}, mode {mode}, call {}: a closure \
                             fetched through its image must compute what the evaluator does \
                             (trained: {trained})",
                            call + 1
                        );
                    }
                }
            }
            code_image::forget(database);
            crate::interp().code_image.build = previous;
            crate::interp().show_error_messages = show;
            crate::truncate_frames(crate::interp(), 0);
            crate::interp().foreign_calls.clear();
            run("rm(g_image, g_train)");
            if let Err(failure) = std::fs::remove_dir_all(&directory) {
                panic!("cannot remove {}: {failure}", directory.display());
            }
            crate::heap().release_handles(mark);
        }
    });
}

/// Writes `object` to `out` in R's XDR serialization, for the plain values a function's formals
/// and body hold. Returns `false`, leaving `out` partly written, for anything else.
unsafe fn xdr_item(out: &mut Vec<u8>, object: Sexp) -> bool {
    unsafe {
        let word = |out: &mut Vec<u8>, value: i32| out.extend_from_slice(&value.to_be_bytes());
        let string = |out: &mut Vec<u8>, charsxp: Sexp| {
            if charsxp == values::na_string() {
                word(out, 9);
                word(out, -1);
            } else {
                let bytes = charsxp.bytes();
                word(out, 9 | 1 << 3 << 12);
                word(out, bytes.len() as i32);
                out.extend_from_slice(bytes);
            }
        };
        if object == values::nil() {
            word(out, 254);
            return true;
        }
        if object == values::global_env() {
            word(out, 253);
            return true;
        }
        if object == crate::heap().known().missing_arg {
            word(out, 251);
            return true;
        }
        let kind = object.kind();
        if kind != SexpType::Symbol && object.attrib() != values::nil() {
            return false;
        }
        match kind {
            SexpType::Symbol => {
                word(out, 1);
                string(out, object.car());
                true
            }
            SexpType::Pairlist | SexpType::Language => {
                let code = if kind == SexpType::Pairlist { 2 } else { 6 };
                let tagged = object.tag() != values::nil();
                word(out, code | i32::from(tagged) << 10);
                (!tagged || xdr_item(out, object.tag()))
                    && xdr_item(out, object.car())
                    && xdr_item(out, object.cdr())
            }
            SexpType::Integer | SexpType::Logical | SexpType::Real | SexpType::String => {
                let code = match kind {
                    SexpType::Logical => 10,
                    SexpType::Integer => 13,
                    SexpType::Real => 14,
                    _ => 16,
                };
                word(out, code);
                let count = values::length(object);
                word(out, count as i32);
                for index in 0..count {
                    match kind {
                        SexpType::Real => {
                            let value = *(object.data() as *const f64).add(index);
                            out.extend_from_slice(&value.to_bits().to_be_bytes());
                        }
                        SexpType::String => string(out, values::string_elt(object, index)),
                        _ => word(out, *(object.data() as *const i32).add(index)),
                    }
                }
                true
            }
            _ => false,
        }
    }
}

/// The deparsed value of the last expression of `text` in the global environment, or `error`.
unsafe fn run(text: &str) -> String {
    unsafe {
        let mark = crate::heap().handle_mark();
        let outcome = (|| -> Result<Sexp, Unwind> {
            let expressions = lower::parse_text(text, "<fuzz>")?;
            let mut value = values::nil();
            for index in 0..values::length(expressions) {
                value = eval::eval(expressions.element(index), values::global_env())?;
            }
            Ok(value)
        })();
        let rendered = match outcome {
            Ok(value) => {
                deparse::deparse_lines(value, 60, deparse::options::KEEP_INTEGER).join("\n")
            }
            Err(_) => "error".to_owned(),
        };
        crate::truncate_frames(crate::interp(), 0);
        crate::interp().foreign_calls.clear();
        crate::heap().release_handles(mark);
        rendered
    }
}

/// Assignments, a loop, and a final expression.
fn program(entropy: &mut impl Entropy) -> String {
    let mut statements = vec![format!("s <- {}", expression(entropy, 2))];
    match entropy.below(4) {
        0 => statements.push(format!(
            "for (i in 1:{}) {{ s <- s + {}; if ({}) next; if ({}) break }}",
            entropy.below(5) + 1,
            expression(entropy, 2),
            condition(entropy),
            condition(entropy)
        )),
        1 => statements.push(format!(
            "k <- 0L\nwhile (k < {}L) {{ k <- k + 1L; s <- {} }}",
            entropy.below(4),
            expression(entropy, 2)
        )),
        2 => statements.push(format!(
            "s <- if ({}) {} else {}",
            condition(entropy),
            expression(entropy, 2),
            expression(entropy, 2)
        )),
        _ => {}
    }
    statements.push(expression(entropy, 3));
    statements.join("\n")
}

const ELEMENT_SUBJECTS: &[&str] = &[
    "1:3",
    "c(1.5, NA, 3)",
    "c(a = 1L, b = 2L, a = 3L)",
    "c(\"p\", NA, \"\")",
    "c(TRUE, NA)",
    "c(x = 1+2i)",
    "list(1, \"x\", NULL)",
    "list(a = 1, b = list(c = 2), 3)",
    "list(a = NULL, ab = 2L)",
    "NULL",
    "as.raw(c(1, 255))",
];

const ELEMENT_INDICES: &[&str] = &[
    "1L",
    "2",
    "2.7",
    "0",
    "-1",
    "NA",
    "5L",
    "\"a\"",
    "\"b\"",
    "\"ab\"",
    "\"\"",
    "NA_character_",
    "c(1, 2)",
    "TRUE",
    "3",
];

/// The values `a` is called with: every element value that does not name a variable, since the
/// reset assigns `a` before any program variable exists.
const ELEMENT_ARGUMENTS: &[&str] = &[
    "1",
    "\"s\"",
    "NULL",
    "list(1)",
    "c(u = 2L)",
    "NA",
    "TRUE",
    "3:1",
];

const ELEMENT_VALUES: &[&str] = &[
    "1",
    "\"s\"",
    "NULL",
    "list(1)",
    "c(u = 2L)",
    "NA",
    "a",
    "x",
    "TRUE",
    "3:1",
];

const ATTRIBUTE_NAMES: &[&str] = &[
    "\"a\"",
    "\"names\"",
    "\"class\"",
    "\"dim\"",
    "\"row.names\"",
    "\"ab\"",
    "\"n\"",
    "\"dimnames\"",
];

/// Starts from a subject `x`, applies a few random reads and writes, and ends with `x`, its
/// attributes and the last value read.
fn element_program(entropy: &mut impl Entropy) -> String {
    fn pick<'a>(entropy: &mut impl Entropy, from: &[&'a str]) -> &'a str {
        from[entropy.below(from.len())]
    }
    let mut statements = vec![
        format!("x <- {}", pick(entropy, ELEMENT_SUBJECTS)),
        "v <- NULL".to_owned(),
        "i <- c(3L, 1L)".to_owned(),
    ];
    for _ in 0..entropy.below(5) + 1 {
        let statement = match entropy.below(13) {
            0 => format!(
                "attr(x, {}) <- {}",
                pick(entropy, ATTRIBUTE_NAMES),
                pick(entropy, ELEMENT_VALUES)
            ),
            1 => format!(
                "names(x) <- {}",
                pick(
                    entropy,
                    &[
                        "NULL",
                        "c(\"p\", \"q\", \"r\")",
                        "\"p\"",
                        "c(NA, \"q\")",
                        "1:2",
                        "b"
                    ]
                )
            ),
            2 => format!(
                "x[[{}]] <- {}",
                pick(entropy, ELEMENT_INDICES),
                pick(entropy, ELEMENT_VALUES)
            ),
            3 => format!("x <- c(x, {})", pick(entropy, ELEMENT_VALUES)),
            4 => format!("v <- x[[{}]]", pick(entropy, ELEMENT_INDICES)),
            5 => format!("v <- .subset2(x, {})", pick(entropy, ELEMENT_INDICES)),
            6 => format!(
                "v <- attr(x, {}{})",
                pick(entropy, ATTRIBUTE_NAMES),
                pick(entropy, &["", ", exact = TRUE", ", TRUE", ", NA"])
            ),
            7 => format!(
                "v <- c({}, {}, {})",
                pick(
                    entropy,
                    &["a", "x", "p = 1", "q = x", "NULL", "list(z = 1)"]
                ),
                pick(entropy, ELEMENT_VALUES),
                pick(
                    entropy,
                    &["b", "r = c(s = 1, 2)", "\"t\"", "NA_integer_", "3i"]
                )
            ),
            8 => format!(
                "v <- c(x, recursive = {})",
                pick(entropy, &["TRUE", "FALSE"])
            ),
            9 => "v <- names(x)".to_owned(),
            10 | 11 => format!(
                "x[{}] <- {}",
                pick(
                    entropy,
                    &[
                        "c(1L, 3L)",
                        "2:3",
                        "c(TRUE, FALSE, TRUE)",
                        "c(1, 1)",
                        "-1",
                        "c(0, 2)",
                        "c(NA, 1)",
                        "4:6",
                        "TRUE",
                        "c(2.9, 1)",
                        "i",
                        "2L",
                        "\"a\"",
                    ]
                ),
                pick(
                    entropy,
                    &[
                        "c(9, 8)",
                        "7L",
                        "c(TRUE, NA)",
                        "\"z\"",
                        "NULL",
                        "c(1L, 2L, 3L)",
                        "2+0i"
                    ]
                )
            ),
            _ => format!(
                "x[[{}]] <- x[[{}]]",
                pick(entropy, ELEMENT_INDICES),
                pick(entropy, ELEMENT_INDICES)
            ),
        };
        statements.push(statement);
    }
    statements.push("list(x, attributes(x), v)".to_owned());
    statements.join("\n")
}

fn expression(entropy: &mut impl Entropy, depth: usize) -> String {
    if depth == 0 || entropy.below(3) == 0 {
        return leaf(entropy);
    }
    match entropy.below(5) {
        0 => format!("-({})", expression(entropy, depth - 1)),
        1 => format!(
            "(if ({}) {} else {})",
            condition(entropy),
            expression(entropy, depth - 1),
            expression(entropy, depth - 1)
        ),
        _ => {
            let operators = ["+", "-", "*", "/", "^", "%/%", "%%", "==", "<", ">=", "!="];
            format!(
                "({} {} {})",
                expression(entropy, depth - 1),
                operators[entropy.below(operators.len())],
                expression(entropy, depth - 1)
            )
        }
    }
}

fn condition(entropy: &mut impl Entropy) -> String {
    match entropy.below(4) {
        0 => format!("{} && {}", comparison(entropy), comparison(entropy)),
        1 => format!("{} || {}", comparison(entropy), comparison(entropy)),
        _ => comparison(entropy),
    }
}

fn comparison(entropy: &mut impl Entropy) -> String {
    let operators = ["<", ">", "==", "!=", "<=", ">="];
    format!(
        "{} {} {}",
        leaf(entropy),
        operators[entropy.below(operators.len())],
        leaf(entropy)
    )
}

const LEAVES: [&str; 12] = [
    "a", "b", "s", "1L", "2", "0.5", "NA", "TRUE", "-1L", "1e308", "0L", "3L",
];

fn leaf(entropy: &mut impl Entropy) -> String {
    LEAVES[entropy.below(LEAVES.len())].to_owned()
}

/// Renders a random drawing to every format twice: rendering must not panic, must give the same
/// bytes both times, and must give one file per page except for PDF and single-file
/// PostScript. Drawings nest definitions, refer to definitions that are missing or of the wrong
/// kind, and carry degenerate geometry such as empty outlines, zero-size tiles, coincident
/// gradient ends and non-finite coordinates.
pub fn check_render_drawing(entropy: &mut impl Entropy) {
    let drawing = drawing(entropy);
    let size = [0.5 + entropy.below(4) as f64, 0.5 + entropy.below(4) as f64];
    let formats = [
        Format::Pdf,
        Format::Svg,
        Format::Png,
        Format::Bmp,
        Format::Tiff,
        Format::Jpeg,
        Format::Postscript {
            onefile: true,
            paper: None,
            landscape: false,
        },
        Format::Postscript {
            onefile: false,
            paper: None,
            landscape: true,
        },
    ];
    for format in formats {
        let render = || render_files(format, &drawing, size, 24.0, "fuzz", 75);
        let first = render();
        let second = render();
        match (&first, &second) {
            (Ok(first), Ok(second)) => {
                assert!(first == second, "rendering is not deterministic");
                let single = matches!(
                    format,
                    Format::Pdf | Format::Postscript { onefile: true, .. }
                );
                let expected = if single { 1 } else { drawing.pages.len() };
                assert_eq!(first.len(), expected, "one file per page");
                for file in first {
                    check_file_structure(format, file, drawing.pages.len());
                }
            }
            (Err(first), Err(second)) => assert_eq!(first.to_string(), second.to_string()),
            _ => panic!("rendering succeeded only once"),
        }
    }
}

/// Checks that `file` is a well-formed file of `format`, as a reader would parse it: a PDF's
/// cross-reference table points at its objects and its page tree counts every page, an SVG's
/// tags nest, a PNG decodes, and the other formats carry their framing and sizes.
fn check_file_structure(format: Format, file: &[u8], pages: usize) {
    let text = String::from_utf8_lossy(file);
    match format {
        Format::Pdf => {
            assert!(file.starts_with(b"%PDF-"), "a PDF starts with its header");
            assert!(text.trim_end().ends_with("%%EOF"), "a PDF ends with %%EOF");
            let start = text.rfind("startxref").expect("a PDF names its xref table");
            let offset: usize = text[start + "startxref".len()..]
                .split_whitespace()
                .next()
                .and_then(|offset| offset.parse().ok())
                .expect("startxref gives an offset");
            assert!(
                file.get(offset..)
                    .is_some_and(|rest| rest.starts_with(b"xref")),
                "startxref points at the xref table"
            );
            // The lossy text replaces binary stream bytes, so offsets index the bytes.
            let table = String::from_utf8_lossy(&file[offset..]);
            let mut lines = table.lines().skip(1);
            let header = lines.next().expect("the xref table has a subsection");
            let mut header = header
                .split_whitespace()
                .map(|number| number.parse::<usize>());
            let (Some(Ok(first)), Some(Ok(count))) = (header.next(), header.next()) else {
                panic!("the xref subsection header is two numbers");
            };
            for (number, entry) in (first..first + count).zip(lines) {
                let mut fields = entry.split_whitespace();
                let (Some(position), Some(_), Some(kind)) =
                    (fields.next(), fields.next(), fields.next())
                else {
                    panic!("xref entry {number} has three fields: {entry:?}");
                };
                if kind != "n" {
                    continue;
                }
                let position: usize = position.parse().expect("an xref offset is a number");
                let object = format!("{number} 0 obj");
                assert!(
                    file.get(position..)
                        .is_some_and(|rest| rest.starts_with(object.as_bytes())),
                    "xref entry {number} points at `{object}`"
                );
            }
            let counted = text
                .split("/Count ")
                .nth(1)
                .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|count| count.parse::<usize>().ok());
            assert_eq!(counted, Some(pages), "the page tree counts every page");
        }
        Format::Svg => {
            assert!(text.contains("<svg"), "an SVG has an svg element");
            let mut open: Vec<&str> = Vec::new();
            let mut rest = text.as_ref();
            while let Some(start) = rest.find('<') {
                rest = &rest[start + 1..];
                let end = rest.find('>').expect("every tag closes");
                let tag = &rest[..end];
                rest = &rest[end + 1..];
                if tag.starts_with('?') || tag.starts_with('!') {
                    continue;
                }
                let name = |tag: &'_ str| {
                    tag.split(|c: char| c.is_whitespace() || c == '/')
                        .find(|part| !part.is_empty())
                        .unwrap_or("")
                        .to_owned()
                };
                if let Some(closing) = tag.strip_prefix('/') {
                    let opened = open.pop().expect("a closing tag has an opening one");
                    assert_eq!(name(opened), name(closing), "SVG tags nest");
                } else if !tag.ends_with('/') {
                    open.push(tag);
                }
            }
            assert!(open.is_empty(), "every SVG element closes: {open:?}");
            for (index, _) in text.match_indices('&') {
                let entity = &text[index..text.len().min(index + 8)];
                assert!(
                    ["&amp;", "&lt;", "&gt;", "&quot;", "&apos;", "&#"]
                        .iter()
                        .any(|known| entity.starts_with(known)),
                    "an SVG escapes every ampersand: {entity:?}"
                );
            }
        }
        Format::Png => {
            let pixmap = tiny_skia::Pixmap::decode_png(file).expect("the PNG decodes");
            assert!(
                pixmap.width() > 0 && pixmap.height() > 0,
                "a PNG has pixels"
            );
        }
        Format::Bmp => {
            assert!(file.starts_with(b"BM"), "a BMP starts with BM");
            let field = |at: usize| {
                file.get(at..at + 4)
                    .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                    .expect("the BMP header is whole")
            };
            assert_eq!(
                field(2) as usize,
                file.len(),
                "a BMP's header gives its size"
            );
            assert!(
                (field(10) as usize) < file.len(),
                "a BMP's pixels start inside it"
            );
        }
        Format::Tiff => assert!(
            file.starts_with(b"II*\0") || file.starts_with(b"MM\0*"),
            "a TIFF starts with its byte order and magic number"
        ),
        Format::Jpeg => {
            assert!(file.starts_with(&[0xff, 0xd8]), "a JPEG starts with SOI");
            assert!(file.ends_with(&[0xff, 0xd9]), "a JPEG ends with EOI");
        }
        Format::Postscript { onefile, .. } => {
            assert!(
                file.starts_with(b"%!PS-Adobe-"),
                "PostScript starts with its header"
            );
            assert!(
                text.trim_end().ends_with("%%EOF"),
                "PostScript ends with %%EOF"
            );
            let showpages = text.matches("showpage").count();
            let expected = if onefile { pages } else { 1 };
            assert!(showpages >= expected, "every page is shown");
            if onefile {
                let declared = text
                    .lines()
                    .find_map(|line| line.strip_prefix("%%Pages: "))
                    .and_then(|count| count.trim().parse::<usize>().ok());
                assert_eq!(declared, Some(pages), "%%Pages counts every page");
            }
        }
    }
}

fn drawing(entropy: &mut impl Entropy) -> Drawing {
    let mut definitions = Vec::new();
    for _ in 0..entropy.below(6) {
        let definition = definition(entropy, definitions.len());
        definitions.push(definition);
    }
    let pages = (0..1 + entropy.below(2))
        .map(|_| Page {
            background: colour(entropy),
            items: items(entropy, definitions.len(), 6),
        })
        .collect();
    Drawing { pages, definitions }
}

/// A definition that refers only to the `defined` definitions before it, as the device builds
/// them, except that a reference is now and then out of range.
fn definition(entropy: &mut impl Entropy, defined: usize) -> Definition {
    match entropy.below(6) {
        0 => Definition::Pattern(Pattern::Linear {
            start: point(entropy),
            end: point(entropy),
            stops: stops(entropy),
            extend: extend(entropy),
        }),
        1 => Definition::Pattern(Pattern::Radial {
            start: point(entropy),
            start_radius: number(entropy).abs(),
            end: point(entropy),
            end_radius: number(entropy).abs(),
            stops: stops(entropy),
            extend: extend(entropy),
        }),
        2 => {
            let [x0, y0] = point(entropy);
            let [x1, y1] = point(entropy);
            Definition::Pattern(Pattern::Tiling {
                rect: Rect {
                    x0: x0.min(x1),
                    x1: x0.max(x1),
                    y0: y0.min(y1),
                    y1: y0.max(y1),
                },
                items: items(entropy, defined, 3),
                extend: extend(entropy),
            })
        }
        3 => Definition::ClipPath {
            outline: outline(entropy),
            rule: rule(entropy),
        },
        4 => Definition::Mask {
            luminance: entropy.chance(1, 2),
            items: items(entropy, defined, 3),
        },
        _ => Definition::Group {
            destination: items(entropy, defined, 2),
            operator: Operator::from_code(1 + entropy.below(25) as i32).unwrap_or(Operator::Over),
            source: items(entropy, defined, 3),
        },
    }
}

fn items(entropy: &mut impl Entropy, defined: usize, most: usize) -> Vec<Item> {
    (0..entropy.below(most + 1))
        .map(|_| Item {
            clip: if entropy.chance(1, 3) {
                Clip::Path(reference(entropy, defined))
            } else {
                let [x0, y0] = point(entropy);
                let [x1, y1] = point(entropy);
                Clip::Rect(Rect {
                    x0: x0.min(x1),
                    x1: x0.max(x1),
                    y0: y0.min(y1),
                    y1: y0.max(y1),
                })
            },
            mask: entropy.chance(1, 4).then(|| reference(entropy, defined)),
            mark: mark(entropy, defined),
        })
        .collect()
}

fn mark(entropy: &mut impl Entropy, defined: usize) -> Mark {
    match entropy.below(5) {
        0 => Mark::Text {
            origin: point(entropy),
            text: ["", "ry", "Wg ÄÖ", "a\u{301}"][entropy.below(4)].to_owned(),
            points: 1.0 + entropy.below(40) as f64,
            font: 1 + entropy.below(5) as i32,
            rotation: entropy.below(360) as f64,
            hadj: entropy.below(3) as f64 / 2.0,
            colour: colour(entropy),
        },
        4 => {
            let (columns, rows) = (1 + entropy.below(4), 1 + entropy.below(4));
            Mark::Image {
                pixels: (0..columns * rows).map(|_| colour(entropy)).collect(),
                columns,
                rows,
                placement: Affine([
                    number(entropy),
                    number(entropy) / 4.0,
                    number(entropy) / 4.0,
                    number(entropy),
                    number(entropy),
                    number(entropy),
                ]),
                interpolate: entropy.chance(1, 2),
            }
        }
        1 => Mark::Group {
            group: reference(entropy, defined),
            transform: Affine([
                number(entropy),
                number(entropy) / 4.0,
                number(entropy) / 4.0,
                number(entropy),
                number(entropy),
                number(entropy),
            ]),
        },
        _ => Mark::Path {
            outline: outline(entropy),
            fill: match entropy.below(3) {
                0 => None,
                1 => Some(Fill::Colour(colour(entropy))),
                _ => Some(Fill::Pattern(reference(entropy, defined))),
            },
            rule: rule(entropy),
            stroke: entropy.chance(1, 2).then(|| Stroke {
                colour: colour(entropy),
                lwd: entropy.below(8) as f64 / 2.0,
                lty: [0, 0x44, 0x1343, 0xffff_ffff][entropy.below(4)],
                lend: 1 + entropy.below(3) as i32,
                ljoin: 1 + entropy.below(3) as i32,
                lmitre: 1.0 + entropy.below(10) as f64,
            }),
        },
    }
}

fn outline(entropy: &mut impl Entropy) -> Outline {
    match entropy.below(4) {
        0 => circle_outline(point(entropy), number(entropy).abs()),
        _ => {
            let mut outline = vec![Segment::Move(point(entropy))];
            for _ in 0..entropy.below(6) {
                outline.push(match entropy.below(5) {
                    0 => Segment::Cubic(point(entropy), point(entropy), point(entropy)),
                    1 => Segment::Close,
                    2 => Segment::Move(point(entropy)),
                    _ => Segment::Line(point(entropy)),
                });
            }
            outline
        }
    }
}

fn stops(entropy: &mut impl Entropy) -> Vec<Stop> {
    let mut stops: Vec<Stop> = (0..entropy.below(4))
        .map(|_| Stop {
            offset: entropy.below(5) as f64 / 4.0,
            colour: colour(entropy),
        })
        .collect();
    stops.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    stops
}

/// A definition index, now and then one past the end or far beyond it.
fn reference(entropy: &mut impl Entropy, defined: usize) -> usize {
    match entropy.below(8) {
        0 => defined,
        1 => usize::MAX,
        _ => entropy.below(defined.max(1)),
    }
}

/// A coordinate in inches, usually on the page and now and then far off it or not finite.
fn point(entropy: &mut impl Entropy) -> [f64; 2] {
    [number(entropy), number(entropy)]
}

fn number(entropy: &mut impl Entropy) -> f64 {
    match entropy.below(20) {
        0 => f64::NAN,
        1 => f64::INFINITY,
        2 => -1e9,
        3 => 0.0,
        _ => entropy.below(400) as f64 / 100.0 - 0.5,
    }
}

fn colour(entropy: &mut impl Entropy) -> u32 {
    let alpha = [0, 0x80, 0xff][entropy.below(3)];
    (alpha << 24) | (entropy.next_u64() as u32 & 0x00ff_ffff)
}

fn extend(entropy: &mut impl Entropy) -> Extend {
    [Extend::Pad, Extend::Repeat, Extend::Reflect, Extend::None][entropy.below(4)]
}

fn rule(entropy: &mut impl Entropy) -> FillRule {
    if entropy.chance(1, 2) {
        FillRule::Winding
    } else {
        FillRule::EvenOdd
    }
}
