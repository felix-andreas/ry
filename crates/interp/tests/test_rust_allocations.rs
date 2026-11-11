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
formatted <- function(x) .Internal(sprintf('%s has %5.2f and %-4d items', c('a', 'b'), x, 3L))
pasted <- function(x) .Internal(paste(list(c('a', 'b'), x), '-', NULL, FALSE))
collapsed <- function(x) .Internal(paste0(list(c('a', 'b'), x), ', ', FALSE))
swapped <- function(x) .Internal(gsub('([a-z]+) ([a-z]+)', '\\\\2-\\\\1', x, FALSE, FALSE, FALSE, FALSE))
dotted <- function(x) .Internal(gsub('.', '_', x, FALSE, FALSE, TRUE, FALSE))
guarded <- function(x) {
  here <- .Internal(environment(NULL))
  .Internal(.addCondHands('error', list(function(e) 1), here, here, FALSE))
  x
}
caught <- function() {
  here <- .Internal(environment(NULL))
  hit <- 0
  .Internal(.addCondHands('custom', list(function(condition) hit <<- hit + 1), here, here, TRUE))
  condition <- list(message = 'm', call = NULL)
  class(condition) <- c('custom', 'condition')
  .Internal(.signalCondition(condition, 'm', NULL))
  hit
}
grid <- 1:12
dim(grid) <- c(3L, 4L)
corner <- function(m) m[1:2, 2:3]
as_text <- function(x) .Internal(as.vector(x, 'character'))
via_do_call <- function(x) .Internal(do.call(identity_of, list(x), globalenv()))
paired <- function(x) .Internal(mapply(function(a, b) a + b, list(x, 2:3), NULL))
global_by_name <- function() as.environment('.GlobalEnv')
same <- function(x, y) .Internal(identical(x, y, TRUE, TRUE, TRUE, TRUE, FALSE, TRUE, FALSE))
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
    ("sprintf", "formatted(1.5)", 0),
    ("paste", "pasted('c')", 0),
    ("paste0 with collapse", "collapsed('c')", 0),
    // The regex engine allocates the capture slots of each match itself.
    ("gsub with groups", "swapped('hello world')", 1),
    ("gsub with a fixed pattern", "dotted('a.b.c')", 0),
    ("establish a handler", "guarded(1)", 0),
    ("matrix subset", "corner(grid)", 0),
    ("double to character", "as_text(c(1.5, 2, 1e-20))", 0),
    ("do.call", "via_do_call(1)", 0),
    // Each of the two closure calls `apply` makes from the builtin builds its call and grows a
    // frame through the call protocol, which allocates twice.
    ("mapply", "paired(1:2)", 4),
    ("search path by name", "global_by_name()", 0),
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

/// Checks that must hold while every allocation collects, for the paths that link pairlists cell
/// by cell and reuse pooled handlers without pushing a root of their own.
const STRESSED: &[&str] = &[
    "same(copy_call(quote(f(a, b = 2, c))), quote(f(1, b = 2, c)))",
    "same(listed(quote(f(a, b = 2, c))), list(quote(f), quote(a), b = 2, quote(c)))",
    "same(.Internal(as.vector(.Internal(as.vector(c(a = 1, b = 2, c = 3), 'pairlist')), 'list')), \
     list(a = 1, b = 2, c = 3))",
    "{ total <- 0; for (i in 1:20) total <- total + caught(); total == 20 }",
    "{ for (i in 1:20) guarded(1); TRUE }",
    "same(add(classed), classed)",
    "{ want <- c(4L, 5L, 7L, 8L); dim(want) <- c(2L, 2L); same(corner(grid), want) }",
    "same(as_text(c(1.5, 2, 1e-20)), c('1.5', '2', '1e-20'))",
    "same(via_do_call(5), 5)",
    "same(paired(1:2), list(3L, 5L))",
];

#[test]
fn hot_paths_survive_gc_stress() {
    with_interpreter(|| {
        // SAFETY: the interpreter is installed, and every handle comes from its heap.
        unsafe {
            evaluate(SETUP).unwrap_or_else(|failure| panic!("the setup failed: {failure}"));
            let mut failed = Vec::new();
            for &code in STRESSED {
                let expression = parse(code);
                evaluate(".Internal(gctorture(TRUE))")
                    .unwrap_or_else(|failure| panic!("gctorture failed: {failure}"));
                let result = eval::eval(expression, values::global_env());
                evaluate(".Internal(gctorture(FALSE))")
                    .unwrap_or_else(|failure| panic!("gctorture failed: {failure}"));
                match result {
                    Ok(value) if values::is_true(value) => {}
                    Ok(_) => failed.push(format!("`{code}` was not TRUE")),
                    Err(failure) => failed.push(format!("`{code}` failed: {}", message(failure))),
                }
            }
            assert!(failed.is_empty(), "under GC stress:\n{}", failed.join("\n"));
        }
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
        other => format!("{other:?}"),
    }
}
