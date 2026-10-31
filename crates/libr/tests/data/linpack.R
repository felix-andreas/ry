# Records GNU R's answers for the QR routines behind qr(), lm.fit() and the qr.* helpers.
#
#   Rscript linpack.R <output file>
#
# Each case starts with "case <name> <n> <p> <ny> <tol>", then one tab-separated line per
# vector: the inputs "x" and "y", the "dqrdc2_*" outputs as qr(x, tol, LAPACK = FALSE)
# returns them, the "dqrls_*" outputs from lm.fit()'s native routine, and, when the rank is
# positive, the "dqrqy", "dqrqty", "dqrcf", "dqrrsd" and "dqrxb" results applied to the
# dqrdc2 decomposition, with "*_y" giving `y` as each routine leaves it.
args <- commandArgs(trailingOnly = TRUE)
lines <- character()
emit <- function(label, values) {
  if (is.integer(values)) {
    text <- as.character(values)
  } else {
    text <- sprintf("%.17g", as.double(values))
  }
  lines <<- c(lines, paste(c(label, text), collapse = "\t"))
}

record <- function(name, x, y, tol = 1e-7) {
  storage.mode(x) <- "double"
  y <- as.matrix(y)
  storage.mode(y) <- "double"
  n <- nrow(x); p <- ncol(x); ny <- ncol(y)
  lines <<- c(lines, sprintf("case\t%s\t%d\t%d\t%d\t%.17g", name, n, p, ny, tol))
  emit("x", x)
  emit("y", y)
  q <- qr(x, tol = tol, LAPACK = FALSE)
  emit("dqrdc2_qr", q$qr)
  emit("dqrdc2_qraux", q$qraux)
  emit("dqrdc2_rank", as.integer(q$rank))
  emit("dqrdc2_pivot", q$pivot)
  fit <- .Call(stats:::C_Cdqrls, x, y, tol, FALSE)
  emit("dqrls_qr", fit$qr)
  emit("dqrls_coefficients", fit$coefficients)
  emit("dqrls_residuals", fit$residuals)
  emit("dqrls_effects", fit$effects)
  emit("dqrls_rank", as.integer(fit$rank))
  emit("dqrls_pivot", fit$pivot)
  emit("dqrls_qraux", fit$qraux)
  k <- as.integer(q$rank)
  if (k == 0L) return(invisible())
  for (routine in c("dqrqy", "dqrqty", "dqrrsd", "dqrxb")) {
    entry <- get(paste0(".F_", routine), envir = asNamespace("base"))
    out <- .Fortran(entry, q$qr, n, k, q$qraux, y = y, ny, out = y)
    emit(routine, out$out)
    emit(paste0(routine, "_y"), out$y)
  }
  out <- .Fortran(base:::.F_dqrcf, q$qr, n, k, q$qraux, y = y, ny,
                  coef = matrix(0, k, ny), info = integer(1L))
  emit("dqrcf", out$coef)
  emit("dqrcf_y", out$y)
  emit("dqrcf_info", out$info)
}

set.seed(20261031)
gaussian <- function(n, p) matrix(rnorm(n * p), n, p)

record("square", gaussian(4, 4), rnorm(4))
record("tall", gaussian(12, 5), gaussian(12, 2))
record("tall_many_responses", gaussian(30, 7), gaussian(30, 3))
record("wide", gaussian(3, 5), rnorm(3))
record("one_row", matrix(c(2, -1, 3), 1, 3), 4)
record("one_by_one", matrix(-3, 1, 1), 6)
record("one_column", gaussian(6, 1), rnorm(6))
record("single_row_leading_zeros", matrix(c(0, 0, 0, 2), 1, 4), 1)
record("zero_matrix", matrix(0, 4, 3), rnorm(4))
record("zero_first_column", cbind(0, 1:5, rnorm(5)), rnorm(5))
record("zero_column_tol_zero", cbind(0, 1:5, rnorm(5)), rnorm(5), tol = 0)
record("all_collinear", outer(1:5, c(1, 2, -3, 0.5)), rnorm(5))

x <- gaussian(10, 4); x[, 3] <- x[, 1] + x[, 2]
record("sum_of_columns", x, rnorm(10))
x <- gaussian(8, 5); x[, 2] <- 3 * x[, 1]; x[, 5] <- x[, 3] - x[, 4]
record("two_dependencies", x, gaussian(8, 2))
x <- gaussian(6, 4); x[, 1] <- 2 * x[, 4]
record("first_column_depends_on_last", x, rnorm(6))
x <- gaussian(3, 4); x[, 2] <- x[, 1]
record("wide_collinear", x, rnorm(3))
record("two_rows_collinear_at_last_row", cbind(c(1, 1), c(2, 2), c(1, 3)), c(1, 2))

group <- factor(rep(c("a", "b", "c"), length.out = 15))
dummies <- cbind(1, model.matrix(~ group - 1), rnorm(15))
record("intercept_and_all_dummies", dummies, rnorm(15))
time <- 1:20
record("polynomial", cbind(1, time, time^2, time^3, time^4), rnorm(20))

for (index in 1:6) {
  n <- sample(5:25, 1); p <- sample(2:8, 1); r <- sample(1:min(n, p), 1)
  x <- gaussian(n, r) %*% gaussian(r, p)
  record(sprintf("random_rank_%d_of_%d_%d", r, p, index), x, gaussian(n, 2))
}

# Columns that are a multiple of the tolerance away from depending on the first one, so the
# pivoting decision is close to the boundary.
base <- gaussian(9, 2)
noise <- rnorm(9)
for (scale in c(1e-9, 3e-8, 9e-8, 1e-7, 1.1e-7, 2e-7, 1e-6)) {
  x <- cbind(base, base[, 1] + scale * noise, rnorm(9))
  record(sprintf("near_tolerance_%g", scale), x, rnorm(9))
}
# Bisects for the scale where the third column stops being dropped, then records a scale a
# millionth below it and one a millionth above. Closer than that, the decision hangs on how
# the BLAS GNU R links against rounds the column norms, not on the algorithm.
near <- function(scale) cbind(base, base[, 1] + scale * noise, rnorm(9))
rank_at <- function(scale) {
  set.seed(7)
  qr(near(scale), LAPACK = FALSE)$rank
}
low <- 3e-8; high <- 9e-8
for (step in 1:45) {
  middle <- (low + high) / 2
  if (rank_at(middle) == rank_at(low)) low <- middle else high <- middle
}
for (scale in c(low * (1 - 1e-6), high * (1 + 1e-6))) {
  set.seed(7)
  x <- near(scale)
  record(sprintf("tolerance_boundary_%.7g", scale), x, rnorm(9))
}
for (tol in c(1e-3, 1e-10)) {
  x <- cbind(base, base[, 1] + 1e-5 * noise)
  record(sprintf("tolerance_%g", tol), x, rnorm(9), tol = tol)
}

# Random designs where a few columns are random combinations of others plus a perturbation
# whose size straddles the tolerance.
for (index in 1:20) {
  n <- sample(3:40, 1); p <- sample(2:10, 1)
  x <- gaussian(n, p)
  for (column in sample(seq_len(p), sample(0:(p - 1), 1))) {
    weights <- rnorm(p) * (seq_len(p) != column)
    x[, column] <- x %*% weights + 10^runif(1, -9, -5) * rnorm(n)
  }
  record(sprintf("perturbed_%d", index), x, gaussian(n, sample(1:3, 1)))
}

record("wide_scale_range", cbind(1e150 * rnorm(6), 1e-150 * rnorm(6), rnorm(6)), rnorm(6))
record("tiny_dependent_column", cbind(rnorm(6), 1e-200 * rnorm(6)), rnorm(6))

writeLines(lines, args[[1]])
