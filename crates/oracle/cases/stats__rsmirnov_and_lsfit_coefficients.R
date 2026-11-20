# rsmirnov's simulated two-sample statistics, and lsfit's coefficients: a named vector for one
# response and a matrix for several.
set.seed(1)
print(rsmirnov(3, sizes = c(5, 7)))
print(rsmirnov(4, sizes = c(4, 6), alternative = "greater"))
x <- cbind(a = c(1, 2, 3, 4, 6), b = c(2, 1, 4, 3, 5))
y <- c(1.5, 2.1, 3.9, 4.2, 6.3)
print(lsfit(x, y)$coefficients)
print(lsfit(x, cbind(u = y, v = rev(y)))$coefficients)
