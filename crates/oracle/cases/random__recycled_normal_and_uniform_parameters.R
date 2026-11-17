# rnorm and runif with recycled parameter vectors draw the same stream as one call per element,
# under every normal generator, and fall back to per-element draws where a parameter set draws none.
for (kind in c("Inversion", "Box-Muller", "Kinderman-Ramage")) {
  RNGkind(normal.kind = kind)
  set.seed(1)
  x <- rnorm(7, c(-1, 0, 2.5), c(1, 3))
  set.seed(1)
  y <- vapply(seq_len(7), function(i) rnorm(1, c(-1, 0, 2.5)[(i - 1) %% 3 + 1], c(1, 3)[(i - 1) %% 2 + 1]), 0)
  cat(kind, identical(x, y), sprintf("%a", x[c(1, 7)]), "\n")
  set.seed(2)
  print(rnorm(5, 1:5, c(1, 0)))
  set.seed(3)
  print(rnorm(4, c(0, Inf), 1))
  set.seed(4)
  print(suppressWarnings(rnorm(4, 0, c(1, NA))))
  print(rnorm(3, 1:3))
}
RNGkind(normal.kind = "default")
# runif with recycled bounds, including an empty interval and a reversed one.
set.seed(5)
x <- runif(6, 0, c(1, 10, 100))
set.seed(5)
y <- vapply(seq_len(6), function(i) runif(1, 0, c(1, 10, 100)[(i - 1) %% 3 + 1]), 0)
cat(identical(x, y), sprintf("%a", x[c(1, 6)]), "\n")
set.seed(6)
print(runif(4, c(0, 1), c(1, 1)))
set.seed(7)
print(suppressWarnings(runif(4, c(0, 2), 1)))
