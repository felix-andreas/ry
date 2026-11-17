# `eval` takes an S4 object that extends "environment" as that environment, for `envir` and for
# `enclos`; `rep` reads `each` as a truncated double; `all`, `any` and the cumulative functions
# refuse an expression vector; `environment<-` with `NULL` on a primitive is silent.
t <- function(e) { r <- withCallingHandlers(tryCatch(e, error = function(e) paste("error:", conditionMessage(e))), warning = function(w) { cat("warning:", conditionMessage(w), "\n"); invokeRestart("muffleWarning") }); cat(paste(deparse(r), collapse = " "), "\n") }
setClass("Z", representation(x = "numeric")); setClass("E", contains = "environment")
e <- new("E"); assign("a", 7, envir = e)
t(eval(quote(1), list(b = 2), new("Z", x = 1))); t(eval(quote(a), list(b = 2), e)); t(eval(quote(a), e)); t(eval(quote(1), new("Z", x = 1))); t(evalq(a + b, list(b = 1), e)); t(eval(quote(b), list(b = 2), NULL)); t(eval(quote(1), list(), 1)); t(eval(quote(1), list(), "a")); t(eval(quote(1), list(), list()))
t(rep(1:2, each = 1e10, length.out = 3)); t(rep(1:2, each = 3e9, times = 0)); t(rep(1:2, each = 1e300)); t(rep(1:2, each = Inf)); t(rep(1:2, each = -0.5)); t(rep(1:2, each = -1.5)); t(rep(1:2, each = 2^31, length.out = 5)); t(rep(1:2, each = "1e10", length.out = 3)); t(rep(1:2, each = 2147483647, length.out = 3)); t(rep(1:2, each = NaN)); t(rep(1:2, each = 1e16, length.out = 2))
f <- sum; t({environment(f) <- NULL; f}); t({environment(f) <- globalenv(); 1})
t(all(expression(TRUE))); t(any(expression(TRUE))); t(cumsum(expression(1))); t(cumprod(expression(1))); t(all(list(TRUE))); t(cumsum(list(1, 2))); t(cummax(expression(1))); t(sum(expression(1))); t(max(expression(1))); t(range(list(1, 2))); t(prod(list(2)))
