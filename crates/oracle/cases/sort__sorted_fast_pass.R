# Inputs that are already in the wanted order, which `sort.int`, `order` and `sort.list` may
# return without sorting: ties, missing values, signed zeros, names and other attributes, both
# directions, and compact sequences.
fp <- function(x, decreasing = FALSE, na.last = TRUE) .Internal(sorted_fpass(x, decreasing, na.last))
print(c(fp(1:10), fp(10:1, TRUE), fp(10:1), fp(c(1L, 2L, 2L, 5L)), fp(c(1L, NA)), fp(NA_integer_)))
print(c(fp(integer()), fp(c(3L, 1L)), fp(TRUE), fp("a"), fp(1:3, NA)))
for (x in list(c(1, 1, 2, 3), c(3, 2, 2, 1), c(1L, 1L, 2L), c(3L, 3L, 1L), c(1, NaN), c(NaN),
               NA_real_, c(-0, 0, 1), c(0, -0), c(1, 2, NA, 4), c(-Inf, 0, Inf), numeric())) {
    print(sort(x)); print(sort(x, decreasing = TRUE)); print(sort(x, na.last = TRUE))
    print(order(x)); print(order(x, decreasing = TRUE)); print(order(x, na.last = FALSE))
    print(sort.list(x)); print(sort.list(x, decreasing = TRUE))
    if (!anyNA(x) && length(x) > 0) {
        print(sort.list(x, method = "quick")); print(sort.list(x, decreasing = TRUE, method = "quick"))
        print(1 / sort(x, method = "quick")); print(1 / sort(x, decreasing = TRUE, method = "quick"))
    }
}
y <- c(a = 1, b = 2, c = 3)
attr(y, "extra") <- "kept?"
print(sort(y)); print(attributes(sort(y))); print(order(y))
m <- matrix(c(1, 2, 3, 4), 2)
print(sort(m)); print(sort(m, decreasing = TRUE)); print(order(m))
z <- sort(c(5, 3, 9, 1))
print(c(fp(z), fp(z, TRUE)))
print(order(z)); print(sort(z, decreasing = TRUE)); print(order(rev(z), decreasing = TRUE))
print(sort(as.integer(c(4, 1, 1, 9)))); print(sort(c(2L, 2L), decreasing = TRUE))
invisible(NULL)
