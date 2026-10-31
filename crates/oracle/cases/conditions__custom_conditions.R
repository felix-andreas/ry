cond <- structure(class = c("myCondition", "condition"), list(message = "custom", call = NULL))
r <- withCallingHandlers(
  { signalCondition(cond); "finished" },
  myCondition = function(c) cat("handler saw:", conditionMessage(c), "\n")
)
r
tryCatch(signalCondition(cond), condition = function(c) class(c))
stopper <- function() stop(structure(class = c("customError", "error", "condition"),
  list(message = "custom failure", call = sys.call(-1))))
tryCatch(stopper(), customError = function(e) paste("custom:", conditionMessage(e)))
simpleCondition("m")
class(simpleWarning("w"))
tryCatch(stop(), error = function(e) conditionMessage(e))
