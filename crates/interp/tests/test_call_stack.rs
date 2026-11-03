//! What reading the call stack costs and shares. `do.call` puts the argument values themselves
//! into the call it evaluates, so `sys.call()` and `sys.calls()` must copy only the call's cells:
//! seeded random argument lists of random sizes are passed through `do.call`, and the heap's
//! allocation reporter (the counter behind `Rprofmem`) must see no vector allocated while the
//! callee reads the stack, however large the arguments. Writing into the returned call must
//! still leave the frame's call, and the caller's values, untouched.
//!
//! The interpreter runs here without base R, so the closures call the internals directly.
//! `FUZZ_ITERS` raises the number of seeds.

use {
    heap::object::Sexp,
    interp::{eval, lower, testing::with_interpreter, unwind::Unwind, values},
    std::sync::atomic::{AtomicUsize, Ordering},
    syntax::testing::{SplitMix64, for_each_seed},
};

/// GNU R's base closures over the internals, which `sys_context` expects to be called through.
const SETUP: &str = "
sc <- function(which = 0L) .Internal(sys.call(which))
scs <- function() .Internal(sys.calls())
probe <- function(...) {
  for (i in 1:4) {
    sc()
    scs()
  }
  NULL
}
mutate <- function(...) {
  first <- sc()
  first[[1L]] <- quote(changed)
  for (k in 2:length(first)) first[[k]][[1L]] <- -1
  sc()
}
";

/// Vectors of at most this many bytes are not reported. The arguments' parts are larger, so a
/// copy of any of them would be.
const THRESHOLD: usize = 1000;

static REPORTED: AtomicUsize = AtomicUsize::new(0);

#[test]
fn reading_the_stack_shares_do_call_arguments() {
    with_interpreter(|| {
        // SAFETY: the interpreter is installed, and every handle comes from its heap.
        unsafe {
            evaluate(SETUP).unwrap_or_else(|failure| panic!("the setup failed: {failure}"));
        }
        for_each_seed(40, |seed| {
            // SAFETY: as above.
            unsafe { check_seed(seed) }
        });
    });
}

unsafe fn check_seed(seed: u64) {
    let mut random = SplitMix64(seed);
    let count = 1 + random.below(4);
    let mut names = Vec::new();
    // SAFETY: the interpreter is installed, and every handle comes from its heap.
    unsafe {
        for index in 0..count {
            let name = format!("a{index}");
            let code = if random.chance(1, 2) {
                format!(
                    "{name} <- .Internal(vector(\"double\", {}L))",
                    200 + random.below(5000)
                )
            } else {
                let elements = 1 + random.below(40);
                format!(
                    "{name} <- .Internal(vector(\"list\", {elements}L))
                     for (i in 1:{elements}) {name}[[i]] <- .Internal(vector(\"double\", {}L))",
                    200 + random.below(2000)
                )
            };
            evaluate(&code).unwrap_or_else(|failure| panic!("`{code}` failed: {failure}"));
            names.push(name);
        }
        let arguments = names.join(", ");

        let mark = interp::heap().handle_mark();
        REPORTED.store(0, Ordering::Relaxed);
        interp::heap().report_allocations(Some(count_allocation), THRESHOLD);
        let probed = evaluate(&format!(
            ".Internal(do.call(probe, list({arguments}), globalenv()))"
        ));
        interp::heap().report_allocations(None, 0);
        probed.unwrap_or_else(|failure| panic!("the probe failed: {failure}"));
        assert_eq!(
            REPORTED.load(Ordering::Relaxed),
            0,
            "sys.call() and sys.calls() allocated vectors for arguments {arguments}"
        );

        let originals: Vec<(Sexp, Fingerprint)> = names
            .iter()
            .map(|name| {
                let value = evaluate(name).unwrap_or_else(|failure| panic!("{failure}"));
                (value, fingerprint(value))
            })
            .collect();
        let second = evaluate(&format!(
            ".Internal(do.call(\"mutate\", list({arguments}), globalenv()))"
        ))
        .unwrap_or_else(|failure| panic!("the mutation failed: {failure}"));
        let mutate =
            values::install("mutate").unwrap_or_else(|failure| panic!("{}", message(failure)));
        assert_eq!(
            second.car(),
            mutate,
            "renaming the returned call renamed the frame's call"
        );
        let mut cell = second.cdr();
        for (name, (original, before)) in names.iter().zip(&originals) {
            assert_eq!(
                cell.car(),
                *original,
                "the frame's call lost the value of `{name}`"
            );
            assert_eq!(
                fingerprint(*original),
                *before,
                "writing into the returned call changed `{name}` in place"
            );
            cell = cell.cdr();
        }
        interp::heap().release_handles(mark);
    }
}

/// What a write to element 1 changes: a double's value, or a list's element.
#[derive(Debug, PartialEq)]
enum Fingerprint {
    Double(f64),
    Element(Sexp),
}

unsafe fn fingerprint(value: Sexp) -> Fingerprint {
    // SAFETY: the caller passes a live double vector or list.
    unsafe {
        if value.kind() == heap::object::SexpType::List {
            Fingerprint::Element(value.element(0))
        } else {
            Fingerprint::Double(values::as_real_scalar(value))
        }
    }
}

fn count_allocation(_size: usize) -> bool {
    REPORTED.fetch_add(1, Ordering::Relaxed);
    true
}

/// Evaluates `text` in the global environment and returns the last value, or the error message.
unsafe fn evaluate(text: &str) -> Result<Sexp, String> {
    // SAFETY: the interpreter is installed, and every handle comes from its heap.
    unsafe {
        let expressions = lower::parse_text(text, "<test>").map_err(message)?;
        let mut value = values::nil();
        for index in 0..values::length(expressions) {
            interp::interp().error_message.clear();
            value = eval::eval(expressions.element(index), values::global_env()).map_err(
                |failure| format!("{}{}", message(failure), interp::interp().error_message),
            )?;
        }
        Ok(value)
    }
}

fn message(unwind: Unwind) -> String {
    match unwind {
        Unwind::Error(failure) => failure.message,
        _ => String::new(),
    }
}
