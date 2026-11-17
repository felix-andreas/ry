# A single key already in order, or strictly in the opposite order, skips the radix sort: in
# order it is the identity permutation, and strictly reversed it is the reversed one. Ties, a
# signed zero, NA and NaN must still take the sort, so the stable order of ties and the place of
# NA follow `na.last` and `decreasing` as they do for any other key.
check <- function(x) {
  for (decreasing in c(FALSE, TRUE)) {
    print(order(x, decreasing = decreasing))
    print(order(x, decreasing = decreasing, method = "radix"))
    print(order(x, decreasing = decreasing, na.last = FALSE))
    print(sort(x, decreasing = decreasing))
    print(sort(x, decreasing = decreasing, method = "radix"))
  }
}
check(1:6)
check(6:1)
check(c(10L, 8L, 8L, 3L))
check(c(1L, 3L, 3L, 7L))
check(c(5L, 4L, NA, 2L))
check(c(NA, 4L, 3L))
check(c(2.5, 1, -1, -3))
check(c(-3, -1, 1, 2.5))
check(c(0, -0, -1))
check(c(1, -0, 0))
check(c(3, 2, NaN))
check(c(NaN, 2, 1))
check(c(3, 2, NA, 1))
check(c(Inf, 1, -Inf))
check(c(TRUE, FALSE))
check(numeric(0))
check(7L)
1 / sort(c(0, -0, -1), decreasing = TRUE)
1 / sort(c(-1, -0, 0))
1 / c(-0, 0)[order(c(-0, 0))]
order(c(-0, 0), decreasing = TRUE)
o <- order(c(9L, 7L, 5L, 3L, 1L)); o; typeof(o)
x <- seq(100, 1, by = -1); identical(order(x), 100:1)
x <- seq(1, 100, by = 1); identical(order(x, decreasing = TRUE), 100:1)
