# cat() writes atomic vectors and names. A non-empty list, a call, a pairlist and an expression
# are errors reported against the call to cat(), and an empty list or NULL prints nothing. The
# objects before the bad one, and the separator in front of it, are written before the error.
cat(1:3, "a", TRUE, NA, 1.5, quote(sym), "\n")
cat(list(), NULL, character(), "end\n")
show <- function(expr) {
  message <- tryCatch({ expr; "no error" }, error = function(e) {
    paste(deparse(conditionCall(e)), conditionMessage(e), sep = ": ")
  })
  cat(message, "\n")
}
show(cat(list(1, 2)))
show(cat(list(1)))
show(cat(list(1, "a", TRUE)))
show(cat("x", list(1, 2)))
show(cat(list(NULL)))
show(cat(quote(f(x))))
show(cat(pairlist(1)))
show(cat(expression(1)))
show(cat(sin))
f <- function(x) cat(x)
show(f(list(1)))
show(cat("a", "b", list(1), sep = "-"))
show(cat(NULL, list(1)))
show(cat(1:2, sin, sep = ","))
