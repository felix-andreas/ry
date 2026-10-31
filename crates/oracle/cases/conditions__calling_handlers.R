res <- withCallingHandlers(
  { message("m1"); warning("w1"); "value" },
  message = function(m) { cat("got message:", conditionMessage(m)); invokeRestart("muffleMessage") },
  warning = function(w) { cat("got warning:", conditionMessage(w), "\n"); invokeRestart("muffleWarning") })
res
f <- function() { warning("inner warning"); "f done" }
withCallingHandlers(f(), warning = function(w) { cat("call:", deparse(conditionCall(w)), "\n"); invokeRestart("muffleWarning") })
cond <- simpleCondition("custom msg"); class(cond) <- c("myCondition", "condition")
withCallingHandlers(signalCondition(cond), myCondition = function(c) cat("signalled", conditionMessage(c), "\n"))
tryCatch(signalCondition(cond), condition = function(c) class(c))
result <- tryCatch({ warning("first"); "not reached" }, warning = function(w) paste("handled", conditionMessage(w)))
result
tryCatch(stop("a"), error = function(e) tryCatch(stop("b"), error = function(e2) paste(conditionMessage(e), conditionMessage(e2))))
nested <- function() tryCatch(stop("deep"), finally = cat("finally runs\n"))
tryCatch(nested(), error = function(e) conditionMessage(e))
e <- simpleError("E"); tryCatch(stop(e), error = function(x) identical(x, e))
