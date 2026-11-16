# setTimeLimit() and setSessionTimeLimit() raise an error once evaluation runs past the limit.
# The loops bound themselves at five seconds, so a limit that never fires shows as "no limit"
# instead of hanging, and only the condition message prints because when the check runs varies.
busy <- function(seconds) {
  start <- proc.time()[[3]]
  while (proc.time()[[3]] - start < seconds) NULL
  "no limit"
}
limited <- function(expr) tryCatch(expr, error = conditionMessage)

# An elapsed limit stops a busy loop, and a fired limit is gone.
limited({ setTimeLimit(elapsed = 0.2, transient = TRUE); busy(5) })
limited(busy(0.5))

# A CPU limit counts the time the process computes.
limited({ setTimeLimit(cpu = 0.2, transient = TRUE); busy(5) })

# The limit holds inside a wait loop, raised from the closure that waits.
waits <- function() repeat Sys.sleep(0.01)
tryCatch({ setTimeLimit(elapsed = 0.2, transient = TRUE); waits() },
  error = function(condition) list(conditionMessage(condition), class(condition)))

# A limit outside the positive finite numbers sets none.
limited({ setTimeLimit(elapsed = -1, cpu = Inf, transient = TRUE); busy(0.5) })
limited({ setTimeLimit(elapsed = NA, transient = TRUE); busy(0.5) })

# Limits start again at every top-level expression, which this file's driver does not have, so a
# script shows them. A transient limit ends with its expression; a persistent one restarts until
# setTimeLimit() clears it; a session limit counts from its call, applies from the next
# top-level expression, and is gone once it has fired.
script <- tempfile(fileext = ".R")
writeLines(c(
  "busy <- function(seconds) {",
  "  start <- proc.time()[[3]]",
  "  while (proc.time()[[3]] - start < seconds) NULL",
  "  'no limit'",
  "}",
  "limited <- function(expr) tryCatch(expr, error = conditionMessage)",
  "setTimeLimit(elapsed = 0.3, transient = TRUE)",
  "limited(busy(0.6))",
  "setTimeLimit(elapsed = 1)",
  "limited(busy(0.6))",
  "limited(busy(0.6))",
  "limited(busy(5))",
  "setTimeLimit()",
  "limited(busy(1.5))",
  "setSessionTimeLimit(elapsed = 0.6)",
  "limited(busy(5))",
  "limited(busy(0.6))"
), script)
writeLines(system2(file.path(R.home("bin"), "Rscript"), c("--vanilla", script), stdout = TRUE))
