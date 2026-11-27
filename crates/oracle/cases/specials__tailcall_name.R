# `Tailcall` takes its function as a value or as a one-string name, which it installs as a
# symbol, `NA` included. Any other character vector is called as it is.
f <- function(n) if (n == 0) "done" else Tailcall("f", n - 1)
f(3)
g <- function(n) if (n == 0) "done" else Tailcall(g, n - 1)
g(3)
h <- function() Tailcall(NA_character_)
tryCatch(h(), error = function(e) conditionMessage(e))
k <- function() Tailcall(c("f", "g"), 0)
tryCatch(k(), error = function(e) conditionMessage(e))
