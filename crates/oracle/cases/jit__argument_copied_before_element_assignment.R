# A function that assigns into an element of its argument changes a copy: the caller's value,
# which the argument's promise also holds, stays as it was. Compiled code must copy at the first
# element assignment of any value its frame does not own: a promise's value, a variable an
# inlined callee was passed, or a constant. Each function runs several times so compiled code
# runs it too.
doubled <- function(x) { for (i in seq_along(x)) x[i] <- x[i] * 2; x }
keep <- function(v) { a <- doubled(v); v }
for (k in 1:4) print(keep(c(1, 2)))

counted <- function(x) { n <- length(x); for (i in 1:n) x[i] <- 0; x }
outer_value <- c(5, 6, 7)
for (k in 1:4) { r <- counted(outer_value); print(outer_value) }

listed <- function(x) { x[[1]] <- "changed"; for (i in seq_along(x)) x[[i]] <- i; x }
l <- list("a", "b")
for (k in 1:4) { r <- listed(l); print(l[[1]]) }

through_dots <- function(...) doubled(...)
w <- c(3, 4)
for (k in 1:4) { r <- through_dots(w); print(w) }

matrix_zero <- function(m) { for (i in seq_len(nrow(m))) m[i, 1] <- 0L; m }
mm <- matrix(1:4, 2)
for (k in 1:4) { r <- matrix_zero(mm); print(mm[, 1]) }

caller <- function() {
  v <- c(1, 2, 3)
  out <- doubled(v)
  list(v, out)
}
for (k in 1:4) print(caller())

constant <- function() doubled(c(1, 1))
for (k in 1:4) print(constant())

integer_arg <- function(x) { x[1L] <- x[1L] + 1L; x }
int_caller <- function() { v <- 1:3; r <- integer_arg(v); v }
for (k in 1:4) print(int_caller())

forced_first <- function(x) { force(x); for (i in seq_along(x)) x[i] <- -x[i]; x }
y <- c(8, 9)
for (k in 1:4) { r <- forced_first(y); print(y) }
