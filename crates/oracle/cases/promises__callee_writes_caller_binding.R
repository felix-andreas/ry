# An argument that names a variable is a promise, and the promise reads the variable when the
# callee first uses it. A write to the caller's binding that the callee makes before then is
# therefore visible, whether it comes from `<<-`, from `assign()` into `parent.frame()`, or from
# another function the callee calls. Each function runs several times so its body is compiled
# and its call sites are planned.
x <- 1
f <- function(a) { x <<- 2; a }
for (i in 1:3) { x <- 1; print(f(x)) }
outer <- function() { v <- 1; inner <- function(a) { v <<- 2; a }; inner(v) }
for (i in 1:3) print(outer())
g <- function(a) { assign("y", 20, envir = parent.frame()); a }
run_g <- function() { y <- 10; g(y) }
for (i in 1:3) print(run_g())
writer <- function() assign("z", 300, envir = parent.frame(2))
h <- function(a) { writer(); a }
run_h <- function() { z <- 100; h(z) }
for (i in 1:3) print(run_h())
run_k <- function() {
  w <- 1
  bump <- function() w <<- 5
  k <- function(a) { bump(); a }
  k(w)
}
for (i in 1:3) print(run_k())
# Forcing the promise first reads the value from before the write.
early <- function(a) { force(a); x <<- 3; a }
for (i in 1:3) { x <- 1; print(early(x)) }
# A constant argument is the same either way.
for (i in 1:3) { x <- 1; print(f(1)) }
