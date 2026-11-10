# An argument that is code, passed from a function that keeps its variables in slots, is a
# promise of that code in the caller. Each function below checks one rule that promise must
# keep: it runs once, it sees writes the caller's frame takes before it runs, it can write the
# caller's variables, it is never missing, it outlives the call when the callee keeps it, and
# `substitute` and its kin see the code. Each runs a few times so that its call site settles on
# its fastest calling convention. The second loop runs the same rules from callers that have an
# environment (`environment()` gives one to a function, and the global loop body and `local` have
# one) and through `eval`, and passes constants, which a callee also sees as promises.
twice <- function(x) { x; x }
run_once <- function() { n <- 0; twice({ n <- n + 1; n }); n }
read_late <- function(x) { assign("y", 2, parent.frame()); x }
late <- function() { y <- 1; read_late(y + 10) }
writes <- function() { y <- 1; twice(y <- y + 5); y }
is_missing <- function(a) missing(a)
code_missing <- function() { y <- 1; is_missing(y + 1) }
pass_on <- function(p) twice(p)
nested <- function() { y <- 3; pass_on(y * 2) }
keep <- function(z) function() z
kept <- function() { yy <- 4; get_z <- keep(yy + 1); yy <- 5; get_z() }
unforced <- function(z) 0
never_run <- function() { n <- 0; unforced(n <- 1); n }
label <- function(z) deparse(substitute(z))
labelled <- function() { yy <- 3; label(yy + 1) }
matched <- function(z) match.call()
call_matched <- function() { yy <- 3; deparse(matched(yy * 2)) }
fails <- function(z) tryCatch(z, error = function(e) conditionMessage(e))
failing <- function() { yy <- 1; fails(stop("no ", yy)) }
for (k in 1:3) {
  cat(run_once(), late(), writes(), code_missing(), nested(), kept(), never_run(), labelled(),
    call_matched(), failing(), "\n")
}
env_run_once <- function() { n <- 0; environment(); twice({ n <- n + 1; n }); n }
env_late <- function() { y <- 1; environment(); read_late(y + 10) }
env_label <- function() { yy <- 3; environment(); label(yy + 1) }
env_missing <- function() { environment(); is_missing(1 + 1) }
const_missing <- function() { environment(); is_missing(5) }
env_kept <- function() { yy <- 4; environment(); get_z <- keep(yy + 1); yy <- 5; get_z() }
for (k in 1:3) {
  gv <- k
  cat(env_run_once(), env_late(), env_label(), env_missing(), const_missing(), env_kept(),
    label(gv + 1), label(5), twice(gv * 3), eval(quote(nested())), local(label(k * 2)),
    eval(quote(label(gv - 1)), list(gv = 1)), "\n")
}
