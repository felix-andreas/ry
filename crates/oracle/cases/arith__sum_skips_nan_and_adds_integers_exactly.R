# Within one double argument the first NaN or NA wins; with na.rm the rest still add in long
# double; an integer or logical argument of a double sum totals exactly.
set.seed(1)
x <- runif(1000, -100, 100)
x[sample(1000, 100)] <- NA
x[7] <- NaN
print(sum(x))
print(sum(x, na.rm = TRUE))
print(sum(c(1, NA, NaN, 3)))
print(sum(c(1, NaN, NA, 3)))
print(sum(c(1, NaN, NA, 3), na.rm = TRUE))
print(sum(c(0.1, 0.2, 0.3), na.rm = TRUE) - 0.6)
print(sum(1:10, 0.5))
print(sum(c(TRUE, NA), 1.5))
print(sum(c(TRUE, NA), 1.5, na.rm = TRUE))
print(sum(.Machine$integer.max, 1L, 0.5))
print(sum(c(.Machine$integer.max, .Machine$integer.max), 0.25))
print(sum(NULL, 2.5))
print(sum(NaN, NA_integer_, 1))
print(sum(c(1e308, 1e308), -1e308, na.rm = TRUE))
