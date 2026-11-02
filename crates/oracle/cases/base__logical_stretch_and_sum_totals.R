# A logical subscript longer than the vector stretches it on assignment, even when it selects
# nothing; `sum` over several arguments adds each argument's total in double precision.
x <- 1:2; x[c(FALSE, FALSE, FALSE)] <- 1L; print(x)
x <- 1:2; x[c(FALSE, FALSE, NA)] <- 1L; print(x)
x <- 1:2; x[c(FALSE, FALSE, FALSE)] <- integer(0); print(x)
y <- c(a = 1); y[c(TRUE, FALSE)] <- 2; print(y)
l <- list(1); l[c(FALSE, FALSE, TRUE, FALSE)] <- NULL; print(length(l))
print(rank(c(2, 1, NA, 2, NaN), ties.method = "first"))
print(sum(1e308, 1e308, -1e308))
print(sum(1:3, 2.5, c(1e308, 1e308)))
print(sum(.Machine$integer.max, 1L))
print(format(-0, scientific = TRUE)); op <- options(scipen = -5); print(format(-0)); options(op)
