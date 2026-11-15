# The stack a condition is signalled on includes the call that raised it, also when the
# optimizing tier ran that call without a frame until then. A builtin that fails or warns
# inside such a call must not signal before the call has its frame, or the `error` option's
# handler, a calling handler and the traceback would miss it. Each callee runs a builtin on
# its hot path and gets an argument the builtin rejects only after the callers warmed up.

stack <- function() {
  named <- Filter(function(call) is.name(call[[1L]]), sys.calls())
  names <- vapply(named, function(call) as.character(call[[1L]]), "")
  cat("stack:", paste(names[names != "stack"], collapse = " > "), "\n")
}
options(error = stack)

count <- function(x, k) {
  s <- seq_len(k)
  x + length(s)
}
for (i in 1:30) count(1, 2L)
root <- function(p, k) {
  q <- p + 0
  kk <- k + 0L
  count(q, kk)
}
for (i in 1:60) root(1, 2L)
outer_call <- function() root(1, -1L)
outer_call()
print(root(1, 3L))

narrow <- function(x, k) {
  n <- as.integer(x)
  n + k
}
for (i in 1:30) narrow(2, 1L)
middle <- function(a, k) {
  b <- a * 2
  r <- narrow(b, k)
  r + 1L
}
for (i in 1:30) middle(2, 1L)
top <- function(p, k) {
  t <- p + 1
  middle(t, k)
}
for (i in 1:60) top(2, 1L)

# A warning that `options(warn = 2)` turns into an error is signalled by R code.
options(warn = 2)
top(3e10, 1L)
options(warn = 0)
print(top(2, 1L))

# So is a warning a calling handler takes.
for (i in 1:60) top(2, 1L)
handled <- withCallingHandlers(
  top(3e10, 1L),
  warning = function(w) {
    stack()
    cat("handled:", conditionMessage(w), "\n")
    invokeRestart("muffleWarning")
  }
)
print(handled)

# A warning nothing handles is printed with its call.
for (i in 1:60) top(2, 1L)
print(top(3e10, 1L))
options(warn = 1)
print(top(3e10, 1L))
