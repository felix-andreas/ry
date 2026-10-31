r <- tryCatch({
  warning("careful")
  "not reached"
}, warning = function(w) paste("caught:", conditionMessage(w)),
   finally = cat("cleanup\n"))
r
tryCatch(stop("boom"), error = function(e) conditionMessage(e))
tryCatch(log(-1), warning = function(w) class(w))
res <- try(stop("oops"), silent = TRUE)
class(res)
cat(res)
inherits(attr(res, "condition"), "simpleError")
f <- function() { on.exit(cat("exit 1\n")); on.exit(cat("exit 2\n"), add = TRUE); stop("fail in f") }
tryCatch(f(), error = function(e) deparse(conditionCall(e)))
