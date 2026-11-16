# which.max and which.min skip NaN and NA and keep the first of equal values.
x <- c(3, NaN, 7, NA, 7, -Inf, 2)
print(which.max(x)); print(which.min(x))
print(which.max(c(NA, NaN))); print(which.min(numeric()))
print(which.max(c(-Inf, NA, -Inf))); print(which.min(c(Inf, NaN, Inf)))
print(which.min(c(0, -0))); print(which.max(c(-0, 0)))
print(which.max(c(a = 1, b = 5, c = 5))); print(which.min(c(a = 1, b = 5, c = 1)))
print(which.max(c(2L, NA, 9L, 9L))); print(which.min(c(NA, 4L, 2L, NA)))
print(which.max(c(NA_integer_, NA_integer_))); print(which.min(c(NA, NA)))
print(which.max(c(FALSE, TRUE, NA, TRUE))); print(which.min(c(NA, TRUE, FALSE)))
print(which.max(c(.Machine$integer.max, NA))); print(which.min(c(NA, .Machine$integer.max)))
set.seed(3); y <- runif(5000); y[c(10, 4000)] <- NA
print(which.max(y)); print(which.min(y))
print(which.max(factor(c("b", "a", "c"))))
