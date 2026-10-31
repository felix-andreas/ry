withCallingHandlers(
  { warning("w1"); cat("after w1\n"); "done" },
  warning = function(w) { cat("log:", conditionMessage(w), "\n"); invokeRestart("muffleWarning") }
)
withRestarts(
  { cat("start\n"); invokeRestart("skip", 7); cat("not shown\n") },
  skip = function(v) v * 6
)
computeRestarts()[[1]]$name
f <- function() { warning("from f"); 10 }
f()
suppressWarnings(f())
message("note to stderr")
suppressMessages({ message("hidden"); 1 })
as.integer("x")
warning("top level warning")
