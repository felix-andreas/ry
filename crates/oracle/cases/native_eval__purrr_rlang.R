# oracle: requires purrr, rlang
# Calls that native code evaluates repeatedly: purrr's mapped call and rlang's evaluation of one
# quoted expression in a loop, which run as compiled code from their third evaluation, also
# when the expression changes between evaluations.
library(purrr)
spread <- function(x) max(x) - min(x)
parts <- split(as.numeric(1:60), rep(1:12, each = 5L))
map_dbl(parts, spread)
map_int(parts, length)
map(1:4, function(i) i * 10)
map2_dbl(1:8, 8:1, function(a, b) a * b + 0.5)
pmap_chr(list(a = 1:6, b = letters[1:6]), function(a, b) paste0(b, a))
reduce(1:10, function(acc, x) acc * 2 + x)
captured <- map(1:3, function(i) function() i)
vapply(captured, function(f) f(), 0L)
e <- quote(x * scale + offset)
env <- new.env()
env$scale <- 3
env$offset <- 1
values <- numeric(0)
for (x in 1:10) {
  assign("x", x, envir = env)
  values <- c(values, rlang::eval_bare(e, env))
}
values
results <- vapply(1:10, function(i) rlang::eval_tidy(e, list(x = i, scale = 2, offset = -i)), 0)
results
# The quoted expression changes between evaluations, before and after it compiled.
e <- quote(x + 1)
out <- character(0)
for (i in 1:12) {
  if (i == 5L) e[[3L]] <- 100
  if (i == 8L) e[[1L]] <- as.name("*")
  if (i == 10L) e[[2L]] <- quote(x - 1)
  out <- c(out, format(rlang::eval_bare(e, list2env(list(x = i)))))
}
out
# A frame per evaluation, with `sys.call()`, `parent.frame()` and `environment()` read from a
# function the expression calls.
where <- function() {
  list(call = deparse(sys.call()), parent = exists("marker", envir = parent.frame()))
}
call <- quote(where())
str(lapply(1:4, function(i) rlang::eval_bare(call, list2env(list(marker = i)))))
withVisible(rlang::eval_bare(quote(invisible(5)), environment()))
# Errors and warnings from a late evaluation.
e <- quote(if (x > 6) stop("x is ", x) else x)
tryCatch(
  for (x in 1:10) rlang::eval_bare(e, environment()),
  error = function(err) cat("error:", conditionMessage(err), "| call:", deparse(conditionCall(err)), "\n")
)
e <- quote(if (x > 6) warning("x is ", x) else x)
withCallingHandlers(
  for (x in 1:8) rlang::eval_bare(e, environment()),
  warning = function(w) {
    cat("warning:", conditionMessage(w), "| call:", deparse(conditionCall(w)), "\n")
    invokeRestart("muffleWarning")
  }
)
