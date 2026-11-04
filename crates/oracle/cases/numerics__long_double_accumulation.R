# Cumulative products and sums keep the running total in long double and round each result to
# double, so they differ from a plain double loop and agree with GNU R bit for bit.
set.seed(2)
x <- 1 + runif(1e5) / 100
cat("cumprod equals a plain double loop:", identical(cumprod(x), Reduce(`*`, x, accumulate = TRUE)), "\n")
cat("cumsum equals a plain double loop: ", identical(cumsum(x), Reduce(`+`, x, accumulate = TRUE)), "\n")
print(sprintf("%a", cumprod(x)[c(1, 2, 10, 1000, 1e5)]))
print(sprintf("%a", cumsum(x)[c(1, 2, 10, 1000, 1e5)]))
print(sprintf("%a", cumprod(c(1.1, 2.2, 3.3, 4.4, 5.5, 6.6, 7.7))))
print(cumprod(1:15)); print(sprintf("%a", cumprod(c(a = 3L, b = 7L, c = 11L))))
print(cumprod(c(2, NA, NaN, 3))); print(cumprod(c(2, NaN, NA, 3))); print(cumprod(c(1, 0, Inf, 2)))
print(cumprod(c(1e200, 1e200, 1e-200))); print(cumprod(c(TRUE, TRUE, NA)))
print(cumprod(numeric())); print(cumprod(NULL))
print(cumprod(complex(real = c(1, 2), imaginary = c(1, -1))))
