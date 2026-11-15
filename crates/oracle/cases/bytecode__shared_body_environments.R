# Closures over one body expression share its compiled code, so the code must stay correct for
# every environment, formals list and copy that reaches it.

# A closure made in a loop from the same quoted `function` expression, each in its own
# environment. One environment rebinds `*`, which the compiled code must notice at run time.
maker <- quote(function(x) {
  y <- x * scale
  y + offset
})
closures <- list()
for (k in 1:6) {
  env <- new.env()
  assign("scale", k, envir = env)
  assign("offset", 10 * k, envir = env)
  if (k == 3) assign("*", function(a, b) a - b, envir = env)
  closures[[k]] <- eval(maker, env)
}
for (round in 1:4) print(vapply(closures, function(f) f(round), numeric(1)))
invisible(gc())
late <- new.env()
assign("scale", 0.5, envir = late)
assign("offset", -1, envir = late)
print(eval(maker, late)(8))

# The same body under formals that differ in order, defaults and dots.
shared <- quote({
  a * 10 + b
})
f <- function(a, b) NULL
body(f) <- shared
g <- function(b, a = 100) NULL
body(g) <- shared
h <- function(a, ...) NULL
body(h) <- shared
b <- 7
for (round in 1:3) print(c(f(1, 2), g(1, 2), g(3), h(4)))

# `environment<-` on a closure that has compiled, and on copies of one that has not.
make_counter <- function() {
  count <- 0
  function() {
    count <<- count + 1
    count
  }
}
first <- make_counter()
for (round in 1:3) first()
second <- first
environment(second) <- list2env(list(count = 100))
print(c(second(), second(), first(), second()))
template <- function(x) {
  x * factor + length(extra)
}
copies <- lapply(1:5, function(k) {
  copy <- template
  environment(copy) <- list2env(list(factor = k, extra = seq_len(k)))
  copy
})
for (round in 1:3) print(vapply(copies, function(copy) copy(round), numeric(1)))
