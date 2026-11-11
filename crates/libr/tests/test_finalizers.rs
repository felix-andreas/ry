//! Finalizers under collection stress, through base R's closures (`reg.finalizer()`, `gc()`).
//! Loading base R changes how other native tests resolve a C callable, so these run in their own
//! process.

#![cfg(unix)]

mod harness;

use harness::*;

/// A collection inside `drop_two` finds the first environment dead and queues its finalizer. The
/// finalizer's promise argument keeps `drop_two`'s frame reachable, and that frame binds the second
/// environment, so a queued finalizer that has not run yet keeps the second environment alive. Young
/// stress is what lets the first environment die before `drop_two` returns. `gc()` must run the
/// queue before it collects, or it finalizes one environment instead of two.
#[test]
fn gc_runs_queued_finalizers_before_it_collects() {
    let r = r_with_base();
    r.eval_text(
        "finalized <- 0L
         track <- function(e) reg.finalizer(e, function(e) finalized <<- finalized + 1L)
         drop_two <- function() { e <- new.env(); track(e); e <- new.env(); track(e); NULL }",
    )
    .expect("the setup");
    r.stress_young();
    let finalized = r
        .eval_text("drop_two(); invisible(gc()); finalized")
        .expect("gc() after drop_two()");
    r.stress(false);
    assert_eq!(r.integers(finalized), vec![2]);
}
