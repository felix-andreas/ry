# A callee with a `...` formal takes its call's arguments straight into its slots too: the
# arguments `...` collects become references to the caller's variables and code, held in the
# callee's `...`. Each function below exercises one rule those references keep: `...` counts,
# names and substitutes as a list of promises would, `..1`, `...elt` and `...length` read it,
# an argument forces once however often `list(...)` reads it, a closure that keeps `...` reads
# the caller's variable when it is called, an unused argument is never forced, forwarding
# `...` to another closure, a method or `match.call` keeps the promises, and a frame that gets
# an environment keeps them too. Each runs a few times so its call site settles on its plan.
count <- function(...) length(list(...))
first <- function(...) ..1
named <- function(...) names(list(...))
labels <- function(...) sapply(substitute(list(...))[-1], deparse)
pass <- function(...) count(...)
keep <- function(...) function() list(...)
once <- function(...) { a <- list(...); b <- list(...); length(a) + length(b) }
lazy <- function(x, ...) x
mixed <- function(a, ..., z = 3) c(a, length(list(...)), z)
nth <- function(...) ...elt(2)
len <- function(...) ...length()
gen <- function(x, ...) UseMethod("gen")
gen.default <- function(x, ...) list(...)
mc <- function(...) match.call()
env_dots <- function(...) { e <- environment(); get("...", envir = e); list(...) }
side <- function() {
  n <- 0
  bump <- function() { n <<- n + 1; n }
  r <- once(bump())
  c(r, n)
}
held <- function() { yy <- 1; g <- keep(yy); yy <- 2; g()[[1]] }
for (k in 1:3) {
  v <- k
  cat(count(v, 2, k + 1), first(v, 9), named(a = v, 2), labels(v, k + 1), pass(v, v),
    side(), held(), lazy(1, stop("no")), mixed(1, v, v, z = 4), nth(v, k * 5), len(v, 1, 2),
    length(gen(1, v, 2)), deparse(mc(v, b = 2)), length(env_dots(v, 2)), "\n")
}
f <- function() { yy <- 7; count(yy, yy + 1) + first(yy) }
for (k in 1:3) cat(f(), "\n")
