show <- function(o) cat(sprintf("%a", c(o$par, o$value)), o$counts, o$convergence, o$message, "\n")
fr <- function(x) 100 * (x[2] - x[1]^2)^2 + (1 - x[1])^2
grr <- function(x) c(-400 * x[1] * (x[2] - x[1]^2) - 2 * (1 - x[1]), 200 * (x[2] - x[1]^2))
for (m in c(1, 2, 5, 17)) {
  show(optim(c(-1.2, 1), fr, grr, method = "L-BFGS-B", control = list(lmm = m)))
  show(optim(c(-1.2, 1), fr, grr, method = "L-BFGS-B", lower = c(-2, -1), upper = c(0.8, 0.5), control = list(lmm = m)))
  show(optim(c(-1.2, 1), fr, NULL, method = "L-BFGS-B", lower = c(-Inf, 1.5), control = list(lmm = m)))
  show(optim(c(-1.2, 1), fr, grr, method = "L-BFGS-B", upper = c(Inf, 0.3), control = list(lmm = m)))
}
for (factr in c(0, 1, 1e3, 1e10, 1e15)) show(optim(c(-1.2, 1), fr, grr, method = "L-BFGS-B", control = list(factr = factr)))
for (pgtol in c(1e-12, 1e-3, 1, 50)) show(optim(c(-1.2, 1), fr, grr, method = "L-BFGS-B", control = list(pgtol = pgtol)))
show(optim(c(-1.2, 1), fr, grr, method = "L-BFGS-B", lower = c(-2, -1), upper = c(1, 3), control = list(parscale = c(10, 0.1))))
show(optim(c(-1.2, 1), function(x) -fr(x), function(x) -grr(x), method = "L-BFGS-B", control = list(fnscale = -2)))

# Iteration limit: the driver stops after maxit iterations with code 1.
o <- optim(c(-1.2, 1), fr, grr, method = "L-BFGS-B", control = list(maxit = 4)); print(o)
o <- optim(c(-1.2, 1), fr, grr, method = "L-BFGS-B", control = list(maxit = 0)); show(o)

# Trace levels, including the summaries and the per-iteration reports.
for (t in 1:6) {
  cat("==== trace", t, "\n")
  o <- optim(c(-1.2, 1), fr, grr, method = "L-BFGS-B", lower = c(-2, -1), upper = c(0.8, 3), control = list(trace = t, maxit = 4, REPORT = 2))
  show(o)
}
o <- optim(c(-1.2, 1), fr, grr, method = "L-BFGS-B", control = list(trace = 1, REPORT = 5)); show(o)
o <- optim(c(-5, 2), function(x) sum(x^2), method = "L-BFGS-B", lower = c(-1, -1), control = list(trace = 2)); show(o)

# A gradient that points the wrong way: the line search gives up on the direction.
o <- optim(c(1, 2), function(x) sum(x^2), function(x) -2 * x, method = "L-BFGS-B", control = list(trace = 1)); show(o)

# A linear objective whose last step lands just off a bound ends abnormally in the line search.
cc <- c(-0x1.623366860c768p-8, 0x1.252c4b3d12fd2p-5, -0x1.e9ca17457e223p-16)
o <- optim(c(0x1.9ffa61def34bap-3, 0x1.e280d3ea4d7ap-3, 0x1.287bd0c001e1p-4), function(x) sum(cc * x), function(x) cc,
  method = "L-BFGS-B", lower = c(-0x1.a0de694p-5, -0x1.83cd3b6p-1, -0x1.0d000baap+0),
  upper = c(0x1.0169b656p-1, 0x1.1daee1e4p-1, 0x1.2d555a28p-2), control = list(lmm = 3, trace = 2))
show(o)

# Arguments the method rejects. The returned value is unset in GNU R, so it is not shown.
o <- optim(c(1, 1), fr, grr, method = "L-BFGS-B", lower = c(2, 0), upper = c(1, 3)); print(o[-2])
unset <- function(lines) cat(sub("final  value .*", "final  value <unset>", lines), sep = "\n")
unset(capture.output(o <- optim(c(1, 1), fr, grr, method = "L-BFGS-B", lower = c(2, 0), upper = c(1, 3), control = list(trace = 3)))); print(o[-2])
o <- optim(c(1, 2), fr, method = "L-BFGS-B", control = list(lmm = 0)); print(o[-2])
unset(capture.output(o <- optim(c(1, 2), fr, method = "L-BFGS-B", control = list(factr = -1, trace = 1)))); print(o[-2])
print(tryCatch(optim(c(1, 2), fr, method = "L-BFGS-B", control = list(REPORT = 0)), error = conditionMessage))
print(optim(numeric(0), function(x) 3, method = "L-BFGS-B", control = list(REPORT = 0)))

# Random bounded problems, mixing breakpoints, free set changes and memory sizes.
set.seed(11)
for (k in 1:25) {
  n <- sample(2:12, 1)
  A <- matrix(rnorm(n * n), n); Q <- crossprod(A) + diag(n) * 0.1
  b <- rnorm(n) * 3
  f <- function(x) 0.5 * sum(x * (Q %*% x)) - sum(b * x) + 0.1 * sum(x^4)
  g <- function(x) drop(Q %*% x) - b + 0.4 * x^3
  lo <- -runif(n); up <- runif(n)
  lo[sample(n, 1)] <- -Inf; up[sample(n, 1)] <- Inf
  show(optim(rnorm(n), f, g, method = "L-BFGS-B", lower = lo, upper = up, control = list(lmm = sample(1:7, 1), factr = 10^sample(1:7, 1))))
}
# Nearly linear objectives, where the Cauchy search runs out of curvature.
for (s in c(1e-9, 1e-12)) for (q in c(0, 1e-3)) {
  cc <- c(1, -1.5, s, 0.7, -s / 3)
  show(optim(c(0, 0, 0, 0, 0), function(x) sum(cc * x) + q * sum(x[3:5]^2), function(x) cc + c(0, 0, 2 * q * x[3:5]),
    method = "L-BFGS-B", lower = c(-1, -2, -1, -0.5, -3), upper = c(1, 0.5, 2, 1, 1)))
}
