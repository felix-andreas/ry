# cumsum totals in long double like GNU R's, and NA outranks NaN in every cumulative function.
for (v in list(c(1, NaN, NA, 3), c(1, NA, NaN, 3), c(NaN, 2), c(3, 1, 4, 1, 5), c(1, Inf, -Inf, 2), numeric(), c(1e308, 1e308, -1e308), c(0.1, 0.2, 0.3, -0.6))) {
  print(cumsum(v)); print(cummax(v)); print(cummin(v)); print(sprintf("%.17g", cumsum(v)))
}
set.seed(3); x <- runif(1e5) * 10^sample(-5:5, 1e5, TRUE); print(sum(cumsum(x) != Reduce(`+`, x, accumulate = TRUE)))
print(sum(cumsum(c(0.1, 0.2, 0.3)) != c(0.1, 0.1 + 0.2, 0.1 + 0.2 + 0.3)))

# A signed zero follows the newer element on a tie.
print(1 / cummax(c(-0, 0, -0))); print(1 / cummin(c(0, -0, 0)))
