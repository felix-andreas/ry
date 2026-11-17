# A named complex vector prints each value at its own width but an NA at the common width of
# the values and names.
x <- c(a = 0i, b = NA, alpha = NA); print(x)
x <- c(a = 0i, bbbbbb = NA); print(x)
x <- c(alpha = 1+2i, b = NA, c = 3i); print(x)
x <- c(alpha = NA_complex_, b = NA); print(x)
x <- c(a = 1.5-2i, b = NA, c = -1e10+1i); print(x)
x <- setNames(c(1i, NA, 2i, NA, 3i, NA), c("one", "two", "three", "four", "five", "six")); print(x)
x <- setNames(rep(c(NA, 1+1i), 12), paste0("n", 1:24)); print(x)
x <- c(a = 0i, b = NA); print(x, na.print = "-")
