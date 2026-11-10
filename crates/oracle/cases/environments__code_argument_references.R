# An argument that is code, passed from a function that keeps its variables in slots, is a
# promise of that code in the caller. Each function below checks one rule that promise must
# keep: it runs once, it sees writes the caller's frame takes before it runs, it can write the
# caller's variables, it is never missing, it outlives the call when the callee keeps it, and
# `substitute` and its kin see the code. Each runs a few times so that its call site settles on
# its fastest calling convention.
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
