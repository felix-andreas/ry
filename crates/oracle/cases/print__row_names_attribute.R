# Compact row names (`c(NA, -n)` or `c(NA, n)`) print as `1:n`, the way `attr()` reads them,
# when they are an attribute of something other than a data frame.
x <- 1:3
attr(x, "row.names") <- c(NA_integer_, -3L)
print(x)
y <- list(a = 1)
attr(y, "row.names") <- c(NA_integer_, 2L)
print(y)
z <- 1:2
attr(z, "row.names") <- c("a", "b")
z
