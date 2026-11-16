# An empty argument matched by name leaves its formal to a positional argument: GNU R's
# positional pass tells a formal matched by name from its value, and an empty one has none.
show <- function(expr) {
  out <- tryCatch(expr, error = function(e) {
    paste("error:", conditionMessage(e), "| call:", paste(deparse(conditionCall(e)), collapse = " "))
  })
  print(out)
}

f <- function(a, b) c(a = missing(a), b = missing(b))
g <- function(a, b, ...) list(a = missing(a), b = missing(b), n = ...length())
h <- function(a, b = 5) if (missing(a)) "a missing" else a + b
k <- function(first, second) second
partial <- function(alpha, beta) c(missing(alpha), missing(beta))
after_dots <- function(..., last) c(n = ...length(), last = missing(last))

# Each shape runs several times, so the call site's plan and the compiled tiers see it too.
for (round in 1:4) {
  show(f(a = , 1))
  show(f(b = , 1))
  show(f(, 1))
  show(f(1, a = ))
  show(f(a = , b = 1))
  show(f(a = ))
  show(f(a = , ))
  show(f(a = , b = ))
  show(f(a = , 1, 2))
  show(f(a = , a = 1))
  show(g(a = , 1, 2))
  show(g(b = , 1, 2))
  show(g(b = , 1, 2, 3))
  show(g(z = , 1, 2))
  show(h(a = , 1))
  show(h(b = , 1))
  show(h(b = , 1, 2))
  show(k(first = , 3))
  show(k(second = , 3))
  show(partial(al = , 1))
  show(partial(be = , 1))
  show(after_dots(last = , 1))
}

# The same arguments arriving through `...`, `do.call` and a method.
forward <- function(...) f(...)
forward_g <- function(...) g(...)
for (round in 1:4) {
  show(forward(a = , 1))
  show(forward(1, a = ))
  show(forward(a = 2, 1))
  show(forward_g(b = , 1, 2))
  show(do.call(f, alist(a = , 1)))
  show(do.call(h, alist(b = , 1)))
}
generic <- function(x, ...) UseMethod("generic")
generic.default <- function(x, y, ...) c(x = missing(x), y = missing(y), n = ...length())
for (round in 1:3) {
  show(generic(1, y = , 2))
  show(generic(y = , 1, 2))
}

# Base R functions whose bodies meet the rule.
show(ifelse(test = , 1, 2))
show(ifelse(TRUE, yes = , 2))
show(paste(sep = , "a", "b"))
show(vapply(1:2, FUN.VALUE = , function(i) i * 2, numeric(1)))

# A builtin that matches its arguments by name follows it as well.
show(round(digits = , 2.567))

# An unused argument is reported without an integer's `L`.
two <- function(a, b) a
relay <- function(...) two(...)
show(two(1L, 2L, 4L))
show(two(1L, w = 4L))
show(relay(1L, 2L, 4L, "s"))

# `islistfactor` checks its flag before it looks at the object.
show(unlist(list(a = 1), "a"))
show(unlist(1, "a"))
show(unlist(list(), "a"))
show(unlist(list(a = 1), NA))
show(.Internal(islistfactor(list(a = 1), NA)))
show(.Internal(islistfactor(list(factor("a"), factor("b")), FALSE)))
