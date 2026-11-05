# options(expressions =) bounds the depth of nested calls.
f <- function(n) if (n == 0) 0 else 1 + f(n - 1)
old <- options(expressions = 100)
print(tryCatch(f(200), error = function(e) "too deep"))
options(old)
print(f(200))
print(getOption("expressions"))
