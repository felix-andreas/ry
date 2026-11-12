# A user interrupt that arrives while interrupts are suspended stays pending. Leaving
# suspendInterrupts() does not deliver it: the next statement runs, and the next interrupt check,
# here a wait, signals it. A process with other threads (GNU R's BLAS) can take the signal on one
# of them a little later, so the waits poll for up to five seconds instead of assuming a duration.
events <- character()
tryCatch({
  suspendInterrupts({
    tools::pskill(Sys.getpid(), tools::SIGINT)
    for (i in seq_len(1e4)) i
    events <- c(events, "suspended block done")
  })
  events <- c(events, "after suspendInterrupts")
  for (i in seq_len(500)) Sys.sleep(0.01)
  events <- c(events, "not reached")
}, interrupt = function(condition) events <<- c(events, "interrupt"))
events

# A blocking wait such as Sys.sleep() takes a pending interrupt even while suspended.
events <- character()
tryCatch(
  suspendInterrupts({
    tools::pskill(Sys.getpid(), tools::SIGINT)
    for (i in seq_len(1e4)) i
    events <- c(events, "before Sys.sleep")
    for (i in seq_len(500)) Sys.sleep(0.01)
    events <- c(events, "not reached")
  }),
  interrupt = function(condition) events <<- c(events, "interrupt")
)
events

# The suspension itself, as suspendInterrupts() and allowInterrupts() set and restore it.
states <- c(
  .Internal(interruptsSuspended()),
  suspendInterrupts(.Internal(interruptsSuspended())),
  suspendInterrupts(allowInterrupts(.Internal(interruptsSuspended()))),
  allowInterrupts(.Internal(interruptsSuspended())),
  .Internal(interruptsSuspended())
)
states
