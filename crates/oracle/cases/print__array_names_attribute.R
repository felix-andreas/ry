# An array's names play no part in how it prints, so they print after it as `attr(,"names")`.
x <- matrix(1:4, 2); names(x) <- c("a", "b", "c", "d"); print(x)
x <- matrix(1:4, 2); names(x)[2L] <- "x1"; x
x <- matrix(list(1, "a", TRUE, NULL), 2); names(x) <- c("p", "q", "r", "s"); x
x <- array(1:8, c(2, 2, 2)); names(x) <- letters[1:8]; x
x <- 1:2; attr(x, "dim") <- 2L; attr(x, "names") <- c("a", "b"); x
x <- matrix(c(1i, NA, 2, 3), 2); names(x) <- c("a", "b", "c", "d"); x
x <- matrix(c("u", NA), 1); names(x) <- c("k", NA); attr(x, "tag") <- 1; x
