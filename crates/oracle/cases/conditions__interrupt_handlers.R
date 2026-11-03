# oracle: requires rlang
# A user interrupt is a condition first: handlers see it before it reaches top level.
print(tryCatch(rlang::interrupt(), interrupt = function(e) class(e)))
seen <- NULL
r <- tryCatch(
  withCallingHandlers(rlang::interrupt(), interrupt = function(e) seen <<- "calling"),
  interrupt = function(e) "exiting"
)
print(c(seen, r))

# An interrupt raised while interrupts are suspended stays pending until the next interrupt
# check, here in the loop, rather than firing as suspendInterrupts() returns.
events <- character()
tryCatch({
  suspendInterrupts(rlang::interrupt())
  events <- c(events, "after suspendInterrupts")
  for (i in seq_len(1e5)) i
}, interrupt = function(condition) events <<- c(events, "interrupt"))
events
