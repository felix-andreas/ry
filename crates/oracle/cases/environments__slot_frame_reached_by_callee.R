# A function whose body names no frame-touching call keeps its variables off the heap until
# something asks for its environment. Each route below reaches the caller's frame from a
# callee and changes a variable there; the caller must see the change.
set_parent <- function() assign("x", 10, envir = parent.frame())
via_parent <- function() { x <- 1; set_parent(); x }
eval_parent <- function() eval(quote(x <- 20), parent.frame())
via_eval <- function() { x <- 1; eval_parent(); x }
set_frame <- function(n) assign("x", 30, envir = sys.frame(n))
via_sys_frame <- function() { x <- 1; set_frame(sys.nframe()); x }
via_sys_function <- function(k = 0) {
  x <- 1
  f <- sys.function()
  if (k == 0) x <- f(1) + 39
  x
}
call_text <- function() deparse(sys.call(-1))
via_sys_call <- function(a) { x <- call_text(); x }
via_closure <- function() {
  x <- 1
  bump <- function() x <<- 50
  e <- environment(bump)
  bump()
  c(x, get("x", envir = e))
}
lazy <- function(env) delayedAssign("x", 60, assign.env = env)
via_delayed <- function() { x <- 1; lazy(environment()); x }
via_local <- function() { x <- 1; local(x <- 70, envir = environment()); x }
via_with <- function() { x <- 1; e <- environment(); with(list(e = e), assign("x", 80, envir = e)); x }
via_do_call <- function() { x <- 1; do.call(assign, list("x", 90, envir = environment())); x }
via_rm <- function() { x <- 1; y <- 2; rm("y", envir = environment()); c(x, exists("y", inherits = FALSE)) }
via_on_exit <- function() {
  x <- 1
  out <- new.env()
  g <- function() { on.exit(assign("seen", x, envir = out)); x <- 100; x }
  c(g(), get("seen", envir = out))
}
for (i in 1:60) {
  r <- list(via_parent(), via_eval(), via_sys_frame(), via_sys_function(), via_sys_call(i),
            via_closure(), via_delayed(), via_local(), via_with(), via_do_call(), via_rm(),
            via_on_exit())
}
str(r)
