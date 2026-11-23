# floor, ceiling and trunc keep the sign of a zero result, pass NA, NaN, infinities and
# integral magnitudes through, and round near 2^52 exactly.
edge <- c(0, -0, 0.3, -0.3, 0.5, -0.5, 0.7, -0.7, 1, -1, 1.5, -1.5, 2.5, -2.5,
          2^52 - 0.5, -(2^52 - 0.5), 2^52, -2^52, 2^52 + 1, 2^53 + 2, 1e300, -1e300,
          5e-324, -5e-324, Inf, -Inf, NaN, NA, 4503599627370495.75, 123456789.999999)
set.seed(7)
values <- c(edge, runif(200, -1e6, 1e6), rnorm(200) * 10^sample(-5:17, 200, TRUE))
for (f in list(floor, ceiling, trunc)) cat(sprintf("%a", f(values)), fill = 100)
print(floor(1:5 / 2))
print(ceiling(c(a = -1.2, b = 3.4)))
print(trunc(c(TRUE, NA)))
x <- c(-2.5, 0.5)
x <- ceiling(x)
print(sprintf("%a", x))
