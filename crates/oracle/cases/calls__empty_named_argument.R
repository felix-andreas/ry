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
for (round in 1:3) {
  show(log(x = , 8))
  show(log(base = , 8))
  show(log(8, base = ))
  show(log(, 8))
  show(log(x = , base = 2, 8))
  show(log(base = , 8, 2))
}

# `rep` evaluates its first argument to dispatch on it, whatever its name, and an empty one is
# the error of the calling frame.
in_frame <- function() rep(times = , 1:2)
relay_rep <- function(...) rep(...)
for (round in 1:3) {
  show(rep(times = , 1:2))
  show(rep(x = , 1:2, 2))
  show(rep(each = , 1:2, 2))
  show(in_frame())
  show(relay_rep(times = , 1:2))
  show(rep(1:2, times = ))
  show(rep(1:2, each = , 2))
}

# `NextMethod` patches the method's arguments as GNU R's `patchArgsByActuals` does: an empty
# argument matched by name reads its formal, and the positional argument that filled the
# formal is passed on empty.
nm <- function(x, ...) UseMethod("nm")
nm.default <- function(x, y, ...) c(y = if (missing(y)) NA else y, n = ...length())
nm.k <- function(x, y, ...) NextMethod()
nm3 <- function(x, ...) UseMethod("nm3")
nm3.default <- function(x, y, z) c(missing(y), missing(z))
nm3.k <- function(x, y, z) NextMethod()
nm_set <- function(x, ...) UseMethod("nm_set")
nm_set.default <- function(x, y, ...) c(y = y, n = ...length())
nm_set.k <- function(x, y, ...) {
  y <- 10
  NextMethod()
}
obj <- structure(1, class = "k")
for (round in 1:3) {
  show(nm(y = , obj, 2))
  show(nm(y = , obj, 2, 5))
  show(nm(obj, y = , 2))
  show(nm(obj, 2))
  show(nm3(y = , obj, 2, 3))
  show(nm3(obj, y = , 2))
  show(nm_set(y = , obj, 2))
  show(nm_set(obj, 2, 3))
}

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

# An unused argument is deparsed without its names.
two <- function(a, b) a
show(do.call(two, list(1L, 2L, list(a = 1L, 2L))))
show(round(1L, 2L, c(a = 1L)))

# An unused argument list is deparsed as a pairlist on one line: an empty argument makes it an
# `alist()`, a name that is not syntactic keeps its backticks, and a block keeps its newlines.
for (round in 1:3) {
  show(two(1, w = ))
  show(two(1, 2, ))
  show(two(1, w = , v = 3))
  show(two(1, `my arg` = 2))
  show(two(1, 2, 3, `if` = 4, `a b`))
  show(two(1, 2, {
    x
    y
  }))
  show(two(1, w = {
    x
  }))
  show(round(1, 2, w = ))
  show(round(1, 2, `my arg` = 3))
  show(do.call(two, alist(1, 2, w = )))
}
