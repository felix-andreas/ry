# floor, ceiling and trunc round whole vectors; NA and NaN come back unchanged, and a zero
# result keeps the sign of its operand.
x <- c(-2.5, -0.5, -0, 0, 0.5, 2.5, NA, NaN, Inf, -Inf, 4503599627370495.5,
       -4503599627370497, 1e300, -1e-300, 0.49999999999999994)
for (f in list(floor, ceiling, trunc)) {
  y <- f(x)
  print(sprintf("%a", y))
  print(1 / y[3:4])
  print(is.na(y))
}
long <- rep(x, 40)
print(identical(floor(long), rep(floor(x), 40)))
print(identical(ceiling(long), rep(ceiling(x), 40)))
print(identical(trunc(long), rep(trunc(x), 40)))
print(floor(1:3))
print(ceiling(c(TRUE, NA)))
m <- matrix(c(1.5, -1.5, 2.25, NA), 2, dimnames = list(c("a", "b"), NULL))
print(trunc(m))
