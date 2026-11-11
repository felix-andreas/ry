//! Rust heap allocations on interpreter hot paths. The R heap has its own bump allocator, so a
//! call that only makes R objects must not reach the Rust global allocator: short lists live on
//! the stack, data that already exists is borrowed, and a `return()` or `break` unwinds without
//! boxing. Each shape below runs warm in a loop under a counting allocator, and the number of
//! Rust allocations per iteration must stay within its pinned budget.
//!
//! The interpreter runs here without base R, so the closures call the internals directly.

use {
    heap::object::Sexp,
    interp::{eval, lower, testing::with_interpreter, unwind::Unwind, values},
    std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    },
};

struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn count() {
    // The counter is gone only while the thread is exiting, after every measurement.
    if ALLOCATIONS
        .try_with(|count| count.set(count.get() + 1))
        .is_err()
    {}
}

// SAFETY: every method forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: as above.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count();
        // SAFETY: as above.
        unsafe { System.realloc(pointer, layout, size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: as above.
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const SETUP: &str = "
early <- function(x) return(x)
identity_of <- function(v) v
unwinding <- function(x) { identity_of(return(x)); 2 }
last_of <- function(n) { for (i in 1:n) if (i == n) break; i }
skip <- function(n) { total <- 0; for (i in 1:n) { if (i == 2L) next; total <- total + i }; total }
combine <- function(x) c(x, NULL, 2, 3)
total <- function(x) sum(x, 1, na.rm = TRUE)
copy_call <- function(call) { other <- call; other[[2L]] <- 1; other }
listed <- function(call) .Internal(as.vector(call, 'list'))
classed <- 1
class(classed) <- 'money'
Ops.money <- function(e1, e2) 1
add <- function(x) if (x + x == 1) x else stop('Ops.money did not run')
set_second <- function(x) { x[[2L]] <- 1; x }
";

/// Each shape's code, run once per iteration, and the most Rust allocations it may make.
const SHAPES: &[(&str, &str, usize)] = &[
    ("return", "early(1)", 0),
    ("return through a promise", "unwinding(1)", 0),
    ("break", "last_of(3L)", 0),
    ("next", "skip(3L)", 0),
    ("c() without objects", "combine(1)", 0),
    ("sum() without objects", "total(1)", 0),
    ("duplicate a call", "copy_call(quote(f(a, b = 2, c)))", 0),
    ("call to list", "listed(quote(f(a, b = 2, c)))", 0),
    ("Ops group dispatch", "add(classed)", 0),
    ("[[<- on a list", "set_second(list(1, 2, 3))", 0),
];

const ITERATIONS: usize = 200;

#[test]
fn hot_paths_make_no_rust_allocations() {
    with_interpreter(|| {
        // SAFETY: the interpreter is installed, and every handle comes from its heap.
        unsafe {
            evaluate(SETUP).unwrap_or_else(|failure| panic!("the setup failed: {failure}"));
        }
        let mut over = Vec::new();
        for &(name, code, budget) in SHAPES {
            // SAFETY: as above.
            let expression = unsafe { parse(code) };
            // The first runs compile the closures and grow the frame and handle stacks.
            for _ in 0..3 {
                // SAFETY: as above.
                unsafe { run(expression, code) };
            }
            let before = ALLOCATIONS.with(Cell::get);
            for _ in 0..ITERATIONS {
                // SAFETY: as above.
                unsafe { run(expression, code) };
            }
            let made = ALLOCATIONS.with(Cell::get) - before;
            println!("{name}: {made} Rust allocations in {ITERATIONS} iterations");
            // The heap's own tables grow now and then, which a tenth per iteration absorbs.
            if made > budget * ITERATIONS + ITERATIONS / 10 {
                over.push(format!(
                    "{name} (`{code}`): {made} allocations in {ITERATIONS} iterations, \
                     budget {budget} per iteration"
                ));
            }
        }
        assert!(over.is_empty(), "over budget:\n{}", over.join("\n"));
    });
}

unsafe fn parse(code: &str) -> Sexp {
    // SAFETY: the interpreter is installed.
    unsafe {
        let expressions = lower::parse_text(code, "<test>")
            .unwrap_or_else(|failure| panic!("`{code}` did not parse: {}", message(failure)));
        let expression = expressions.element(0);
        // Keep the parsed code alive across the collections the loop triggers.
        interp::heap().preserve(expression);
        expression
    }
}

unsafe fn run(expression: Sexp, code: &str) {
    // SAFETY: the interpreter is installed, and `expression` is preserved.
    unsafe {
        let mark = interp::heap().handle_mark();
        if let Err(failure) = eval::eval(expression, values::global_env()) {
            panic!("`{code}` failed: {}", message(failure));
        }
        interp::heap().release_handles(mark);
    }
}

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
