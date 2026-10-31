with_retry <- function(expr_fn) withRestarts(expr_fn(), use_value = function(v) v, skip = function() "skipped")
with_retry(function() invokeRestart("use_value", 99))
with_retry(function() invokeRestart("skip"))
f <- function() { r <- computeRestarts(); unlist(lapply(r, function(x) x$name)) }
withRestarts(f(), alpha = function() 1, beta = function() 2)[1:2]
handler <- function(e) invokeRestart("recover", conditionMessage(e))
withCallingHandlers(withRestarts(stop("oops"), recover = function(msg) paste("recovered from", msg)), error = handler)
tryCatch(invokeRestart("nonexistent"), error = function(e) conditionMessage(e))
isRestart(computeRestarts()[[1]])
