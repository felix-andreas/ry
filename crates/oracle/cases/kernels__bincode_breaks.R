# .bincode finds each point's interval by counting the breaks below it (few breaks), by a
# search of fixed length (many), or by GNU R's bisection (breaks holding NaN), and all three
# must agree with GNU R on ties, points equal to a break, both closures and include.lowest.
x <- c(-1, 0, 0.5, 1, 1.5, 2, 2.5, 3, NA, NaN, Inf, -Inf)
b <- c(0, 1, 1, 2, 3)
for (right in c(TRUE, FALSE)) for (lowest in c(TRUE, FALSE)) print(.bincode(x, b, right, lowest))
print(.bincode(c(0, 1, 2, 3), c(0, 1, 2, 3), FALSE, TRUE))
print(.bincode(c(1, 5, 12), c(0, NaN, 10), TRUE, TRUE))
print(.bincode(c(-Inf, -1, 0, 1, 5, 6, Inf), c(-Inf, 0, 0, 0, 5, Inf), FALSE, TRUE))
print(tryCatch(.bincode(1:3, 2, TRUE, TRUE), error = conditionMessage))
print(tryCatch(.bincode(1:3, c(2, 1), TRUE, TRUE), error = conditionMessage))
print(tryCatch(.bincode(1:3, 1:2, NA, TRUE), error = conditionMessage))
set.seed(11)
for (nb in c(2, 3, 5, 63, 64, 65, 200, 1500)) {
  b <- sort(round(runif(nb, -50, 50)))
  x <- c(b, round(runif(2000, -60, 60), 1), NA, NaN, Inf, -Inf)
  for (right in c(TRUE, FALSE)) for (lowest in c(TRUE, FALSE)) {
    codes <- .bincode(x, b, right, lowest)
    cat(nb, right, lowest, sum(codes * seq_along(x), na.rm = TRUE), sum(is.na(codes)), "\n")
  }
}
print(table(cut(c(0, 0.5, 1, 1.5, 2), breaks = c(0, 1, 2), include.lowest = TRUE)))
print(findInterval(c(0, 0.5, 1, 2.5), c(0, 1, 2)))
