//! Interrupts: an interrupt raised while interrupts are suspended, from R code and from native code
//! between `BEGIN_SUSPEND_INTERRUPTS` and `END_SUSPEND_INTERRUPTS`. These tests need base R's
//! closures (`tryCatch()`, `suspendInterrupts()`), and loading base R changes how other native
//! tests resolve a C callable, so they run in their own process.

#![cfg(unix)]

mod harness;

use harness::*;

/// An interrupt raised while interrupts are suspended stays pending, as in GNU R: leaving
/// `suspendInterrupts()` does not deliver it, the next statements run, and the first interrupt
/// check after that (here, in the loop) signals it. `rytest_interrupt` does what
/// `rlang::interrupt()` does.
#[test]
fn an_interrupt_raised_while_suspended_waits_for_the_next_check() {
    let r = r_with_base();
    rytest(&r);
    let run = |source: &str| {
        let parsed = r
            .eval("parse", &[(Some("text"), r.string(source))])
            .expect("the source parses");
        r.eval("eval", &[(None, parsed)])
    };
    let events = run(r#"
        events <- character()
        tryCatch({
          suspendInterrupts(.Call("rytest_interrupt"))
          events <- c(events, "after suspendInterrupts")
          for (i in seq_len(1e5)) i
          events <- c(events, "not reached")
        }, interrupt = function(condition) events <<- c(events, "interrupt"))
        events
    "#)
    .expect("the handler takes the interrupt");
    assert_eq!(
        r.strings(events),
        vec![
            Some("after suspendInterrupts".into()),
            Some("interrupt".into())
        ]
    );

    // Unsuspended, the same call signals at once.
    let events = run(r#"
        tryCatch({
          .Call("rytest_interrupt")
          "not reached"
        }, interrupt = function(condition) "interrupt")
    "#)
    .expect("the handler takes the interrupt");
    assert_eq!(r.strings(events), vec![Some("interrupt".into())]);

    // In C, `R_CheckUserInterrupt` leaves it pending inside the suspended section, and
    // `END_SUSPEND_INTERRUPTS` delivers it.
    let events = run(r#"
        tryCatch(.Call("rytest_suspended_interrupt"), interrupt = function(condition) "interrupt")
    "#)
    .expect("the handler takes the interrupt");
    assert_eq!(r.strings(events), vec![Some("interrupt".into())]);
    let progress = r
        .call("rytest_suspended_progress", &[])
        .expect("the progress");
    assert_eq!(r.integers(progress), vec![1]);

    // An error out of the suspended section lifts the suspension again, as GNU R's contexts
    // restore it.
    let suspended = run(r#"
        tryCatch(.Call("rytest_suspended_error"), error = function(condition) NULL)
        .Internal(interruptsSuspended())
    "#)
    .expect("the error is caught");
    assert_eq!(r.integers(suspended), vec![0]);
}
