# The radix sort's counting pass on short inputs whose keys span few values, with ties kept in
# input order, NA placement, decreasing order and the run boundaries a second key sorts.
set.seed(7)
x <- sample(c(-3:3, NA), 40, replace = TRUE)
print(order(x)); print(order(x, decreasing = TRUE)); print(order(x, na.last = FALSE))
print(order(x, method = "radix", na.last = NA))
y <- sample(5, 40, replace = TRUE)
print(order(x, y)); print(order(y, -seq_along(y)))
print(sort(sample(c(TRUE, FALSE), 30, replace = TRUE)))
print(.Internal(radixsort(TRUE, FALSE, TRUE, TRUE, c(2L, 1L, 2L, 3L, 1L))))
print(order(factor(sample(letters[1:4], 25, replace = TRUE))))
print(order(c(.Machine$integer.max, -.Machine$integer.max, 0L)))
invisible(NULL)
