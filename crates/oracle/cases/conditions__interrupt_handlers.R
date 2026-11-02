# oracle: requires rlang
# A user interrupt is a condition first: handlers see it before it reaches top level.
print(tryCatch(rlang::interrupt(), interrupt = function(e) class(e)))
seen <- NULL
r <- tryCatch(
  withCallingHandlers(rlang::interrupt(), interrupt = function(e) seen <<- "calling"),
  interrupt = function(e) "exiting"
)
print(c(seen, r))
