# cospi, sinpi and tanpi reduce their argument by the period with a truncated quotient, which
# must give GNU R's last bit, signed zeros and NaN warnings at every edge: half and quarter
# turns, huge and subnormal arguments, the infinities and NA.
v <- c(-1e300, -2^53 - 2, -2^52 - 0.5, -1e15 - 0.25, -3, -2.75, -2.5, -1.5, -1, -0.75, -0.5,
       -0.25, -1e-310, -0, 0, 1e-310, 0.1, 0.25, 0.5, 0.75, 1, 1.25, 1.5, 2.5, 3.5, 7.75,
       1e15 + 0.5, 2^52 + 0.5, 2^60, 1e300, Inf, -Inf, NA, NaN)
for (f in c("cospi", "sinpi", "tanpi")) {
  r <- suppressWarnings(get(f)(v))
  cat(f, sprintf("%a", r), "\n")
  cat(f, 1 / r[v == 0], "\n")
}
print(tryCatch(sinpi(Inf), warning = conditionMessage))
set.seed(3)
x <- c(runif(1e4, -1e6, 1e6), rnorm(1e4), round(runif(1e3, -100, 100)) / 4)
for (f in c("cospi", "sinpi", "tanpi")) {
  cat(f, sprintf("%.17g", sum(suppressWarnings(get(f)(x)) * seq_along(x), na.rm = TRUE)), "\n")
}
