# A list with dims prints a summary of each cell, a scalar as its value and anything else as
# its class and length, and an array of three or more dims prints them left-aligned per slice.
x <- array(list(1, 2L, "a", NULL, 1:3, TRUE), c(1, 2, 3)); print(x)
x <- array(list(1, 2L, "a", NULL, 1:3, TRUE), c(1, 2, 3)); dimnames(x) <- list("r", c("p", "q"), NULL); print(x)
x <- array(list(factor("u"), as.raw(1), quote(f(y)), 1i, NA_complex_, list(1, 2)), c(1, 2, 3)); print(x)
x <- array(list(as.raw(1:2), 3:4 + 0i, pairlist(a = 1), sum, quote(y), NA), c(2, 1, 3)); print(x)
x <- matrix(list(factor("u"), as.raw(1), quote(f(y)), 1i, NA_complex_, character(0)), 2); print(x)
x <- matrix(list(Sys.Date, NA_character_, "a long string", 1e10, -0.5, c(a = 1)), 3); print(x)
x <- array(list(1, "b"), c(2, 1, 1, 1)); print(x)
x <- array(list(), c(0, 2, 2)); print(x)
x <- array(list(as.Date("2024-01-02"), structure(1, class = "foo")), c(1, 1, 2)); print(x)
