# A variable passed to a closure is a promise of its name in the caller. Each function below
# exercises one rule that promise must keep: it is forced once, it sees writes the caller's
# frame takes before it is forced, it carries missingness through a chain of calls, it outlives
# the call when the callee keeps it, and `substitute` and its kin see the name. Each runs a few
# times so that its call site settles on its fastest calling convention.
forced_once <- function(x) { x; assign("y", 2, parent.frame()); x }
force_first <- function() { y <- 1; forced_once(y) }
written_first <- function(x) { assign("y", 2, parent.frame()); x }
write_first <- function() { y <- 1; written_first(y) }
is_missing <- function(a) missing(a)
pass_missing <- function(b) is_missing(b)
chain_missing <- function(c) pass_missing(c)
double <- function(q) q * 2
pass_on <- function(p) double(p)
chain_values <- function(v) { w <- v; pass_on(w) }
from_heap <- function(h) { force(environment()); pass_on(h) }
keep <- function(z) function() z
kept <- function() { yy <- 4; get_z <- keep(yy); yy <- 5; get_z() }
own_env <- function(z) environment()
own_env_read <- function() { yy <- 6; e <- own_env(yy); yy <- 7; get("z", envir = e) }
on_exit_read <- function(z) { out <- 0; on.exit(out <- z); out }
on_exit_value <- function() { yy <- 8; on_exit_read(yy) }
delayed <- function(z) { delayedAssign("w", z); function() w }
delayed_value <- function() { yy <- 9; get_w <- delayed(yy); yy <- 10; get_w() }
gen <- function(x) UseMethod("gen")
gen.default <- function(x) function() x
dispatched <- function() { yy <- 11; get_x <- gen(yy); yy <- 12; get_x() }
global_value <- 13
read_global <- function() double(global_value)
for (k in 1:3) {
  cat(force_first(), write_first(), chain_missing(), chain_missing(1), chain_values(k),
    from_heap(k), kept(), own_env_read(), on_exit_value(), delayed_value(), dispatched(),
    read_global(), "\n")
}
label <- function(z) deparse(substitute(z))
labelled <- function() { yy <- 3; label(yy) }
matched <- function(z) match.call()
call_matched <- function() { yy <- 3; matched(yy) }
called <- function(z) sys.call()
call_called <- function() { yy <- 3; called(yy) }
for (k in 1:3) {
  cat(labelled(), deparse(call_matched()), deparse(call_called()), "\n")
}
