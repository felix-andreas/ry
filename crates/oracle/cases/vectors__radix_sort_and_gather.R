# The radix sort behind sort() and order() matches GNU R on NA, NaN, signed zero, ties and
# direction, through both its dense counting pass and its digit passes; plain subsetting
# gathers by integer, double and logical subscripts with NA and out-of-range positions.
x <- c(NaN, NA, 1, -0, 0, NA, NaN, -Inf, Inf, 1)
print(order(x, method = "radix"))
print(order(x, method = "radix", decreasing = TRUE))
print(order(x, method = "radix", na.last = FALSE))
print(order(x, method = "radix", na.last = NA))
print(sort(x, method = "radix", na.last = TRUE))
print(order(c(2L, NA, 1L, 2L), c(3, 1, 2, NA), method = "radix", decreasing = c(TRUE, FALSE)))
print(order(c("b", NA, "a", "b", "B"), method = "radix"))
print(order(c("b", NA, "a", "b", "B"), method = "radix", decreasing = TRUE))
print(.Internal(radixsort(TRUE, FALSE, TRUE, TRUE, c(3L, 1L, 3L, NA))))
print(.Internal(radixsort(TRUE, TRUE, FALSE, FALSE, c("b", "a", "b", NA))))
print(order(c(TRUE, NA, FALSE), as.raw(c(3, 1, 2)), method = "radix"))
print(order(factor(c("z", "a", "m")), method = "radix", decreasing = TRUE))
set.seed(5)
dense <- sample(c(1:50, NA), 5000, replace = TRUE)
wide <- sample(c(-2e9, 2e9, 0L, NA, 7L), 5000, replace = TRUE) + sample.int(1000, 5000, replace = TRUE)
doubles <- c(round(rnorm(5000), 2), NA, NaN, -0, 0, Inf, -Inf)
for (key in list(dense, wide, doubles)) {
  for (decreasing in c(FALSE, TRUE)) for (na.last in c(TRUE, FALSE, NA)) {
    o <- order(key, decreasing = decreasing, na.last = na.last)
    cat(length(o), sum(o * seq_along(o)), "\n")
  }
}
cat(sum(order(dense, doubles[1:5000]) * 1:5000), sum(order(wide, -doubles[1:5000]) * 1:5000), "\n")
print(1 / sort(c(0, -0, 3, -0, 1)))
print(sort(c(2.5, -1, 0.5), decreasing = TRUE))
print(.Internal(sort(c(3, 1, NaN, 2), FALSE)))
v <- c(10.5, 20.5, 30.5, 40.5)
print(v[c(2L, NA, 5L, 1L)]); print(v[c(2.9, NA, 1)]); print(v[c(TRUE, NA, FALSE, TRUE)])
print(v[c(-1L, -3L)]); print(v[c(0L, 2L)]); print(v[c(TRUE, FALSE)]); print((1:4)[c(4L, 4L, 1L)])
print(c(TRUE, FALSE, NA)[c(3, 3, 1, 9)])
