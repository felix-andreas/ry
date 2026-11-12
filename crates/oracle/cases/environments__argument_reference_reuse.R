# Argument references come from a pool that each call returns to when it ends, and forcing one
# whose variable holds a plain value reads the slot directly. These cases check that the value
# read is the one R's promise would give: after the callee writes the caller's variable through
# `parent.frame()`, after nested calls have reused the pool, when an argument list gives up on
# references part way through (`..1`), and when the reference escapes through `sys.function`,
# `substitute`, `match.call` or a closure. Each runs a few times so its call site settles.
written <- function(x) { assign("y", 2, parent.frame()); x }
write_then_read <- function() { y <- 1; written(y) }
twice <- function(a, b) { b; a + b }
nested <- function(a, b) twice(a, twice(b, a))
deep <- function(n, acc) if (n == 0) acc else deep(n - 1, acc + n)
first_dot <- function(x, ...) twice(x, ..1)
dots_then_write <- function(...) { y <- 3; first_dot(y, ...) }
keep <- function(a, b) function() a + b
kept_after_dots <- function(...) { y <- 4; get <- keep(y, ..1); y <- 40; get() }
sys_fun <- function(x) sys.function()
own_function <- function() { y <- 5; deparse(body(sys_fun(y))) }
label <- function(x, y) c(deparse(substitute(x)), deparse(substitute(y)))
labels <- function(...) { y <- 6; label(y, ..1) }
matched <- function(x, ...) match.call()
call_matched <- function() { y <- 7; deparse(matched(y, y + 1)) }
recursive_force <- function(x, n) if (n == 0) x else recursive_force(x, n - 1) + x
for (k in 1:3) {
  cat(write_then_read(), nested(k, 2), deep(30, k), dots_then_write(k), kept_after_dots(k),
    own_function(), labels(k), call_matched(), recursive_force(k, 20), "\n")
}
# A caller with an environment passes compiled code as a reference that keeps the code beside
# it; once forced and released, the next plain variable reusing that reference must not run it.
id <- function(v) v
reified <- function() {
  e <- environment()
  a <- 10
  b <- 20
  c(id(a + 1), id(b), id(a * 2), id(b))
}
for (k in 1:3) cat(reified(), "\n")
