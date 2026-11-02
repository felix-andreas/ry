# nlminb runs PORT's own drivers, so its iterates, counts, codes and trace output match GNU R
# to the last bit. Each problem prints every evaluated point in hex, then the full result.
show <- function(fit) {
  cat("par", sprintf("%a", fit$par), "\n")
  cat("objective", sprintf("%a", fit$objective), "\n")
  cat("convergence", fit$convergence, fit$message, "\n")
  cat("iterations", fit$iterations, "evaluations", fit$evaluations, "\n")
}
logged <- function(f, tag) function(x, ...) {
  cat(tag, sprintf("%a", x), "\n")
  f(x, ...)
}
rosen <- function(p) 100 * (p[2] - p[1]^2)^2 + (1 - p[1])^2
rosen_gradient <- function(p) c(-400 * p[1] * (p[2] - p[1]^2) - 2 * (1 - p[1]), 200 * (p[2] - p[1]^2))
rosen_hessian <- function(p) matrix(c(1200 * p[1]^2 - 400 * p[2] + 2, -400 * p[1], -400 * p[1], 200), 2)

# Unbounded: finite differences, gradient, gradient and Hessian.
show(nlminb(c(-1.2, 1), logged(rosen, "f")))
show(nlminb(c(-1.2, 1), logged(rosen, "f"), logged(rosen_gradient, "g")))
show(nlminb(c(-1.2, 1), logged(rosen, "f"), logged(rosen_gradient, "g"), logged(rosen_hessian, "h")))

# Bounded, with the bound on x[1] active at the solution.
show(nlminb(c(-1.2, 1), logged(rosen, "f"), lower = c(-2, -2), upper = c(0.5, 2)))
show(nlminb(c(-1.2, 1), logged(rosen, "f"), logged(rosen_gradient, "g"), lower = c(-2, -2), upper = c(0.5, 2)))
show(nlminb(c(-1.2, 1), logged(rosen, "f"), rosen_gradient, rosen_hessian, lower = c(-2, -2), upper = c(0.5, 2)))

# A quadratic whose minimum lies outside the box in two coordinates.
target <- c(3, -1, 2, -4)
quadratic <- function(p) sum((1:4) * (p - target)^2) + p[1] * p[2]
quadratic_gradient <- function(p) 2 * (1:4) * (p - target) + c(p[2], p[1], 0, 0)
quadratic_hessian <- function(p) diag(2 * (1:4)) + rbind(c(0, 1, 0, 0), c(1, 0, 0, 0), 0, 0)
show(nlminb(c(1, 1, 1, 1), quadratic, lower = 0, upper = c(2, 5, 5, 5)))
show(nlminb(c(1, 1, 1, 1), quadratic, quadratic_gradient, lower = 0, upper = c(2, 5, 5, 5)))
show(nlminb(c(1, 1, 1, 1), quadratic, quadratic_gradient, quadratic_hessian, lower = 0, upper = c(2, 5, 5, 5)))
show(nlminb(c(1, 1, 1, 1), quadratic, quadratic_gradient, quadratic_hessian))

# A normal likelihood in log-scale parameters, as nlme's variance components are fitted.
y <- c(1.2, 3.4, 2.2, 5.1, 3.3, 4.8, 2.9, 4.1, 3.7, 2.5)
group <- rep(1:5, each = 2)
likelihood <- function(p) {
  between <- exp(2 * p[2]); within <- exp(2 * p[3])
  total <- 0
  for (g in 1:5) {
    r <- y[group == g] - p[1]
    v <- matrix(between, 2, 2) + diag(within, 2)
    total <- total + 0.5 * (determinant(v)$modulus + sum(r * solve(v, r)))
  }
  as.numeric(total)
}
show(nlminb(c(0, 0, 0), likelihood))
show(nlminb(c(0, 0, 0), likelihood, scale = c(1, 2, 0.5)))
show(nlminb(c(0, 0, 0), likelihood, lower = c(-Inf, -3, -3), upper = c(Inf, 3, 0)))

# Trace output.
fit <- nlminb(c(-1.2, 1), rosen, rosen_gradient, control = list(trace = 1))
fit <- nlminb(c(-1.2, 1), rosen, control = list(trace = 5), lower = -2, upper = 0.5)
fit <- nlminb(c(1, 1, 1, 1), quadratic, quadratic_gradient, quadratic_hessian, control = list(trace = 2))

# Iteration and evaluation limits.
show(nlminb(c(-1.2, 1), rosen, control = list(iter.max = 4)))
show(nlminb(c(-1.2, 1), rosen, rosen_gradient, control = list(eval.max = 7)))
show(nlminb(c(-1.2, 1), rosen, rosen_gradient, rosen_hessian, control = list(iter.max = 5, eval.max = 6)))

# A non-finite objective: the step is cut back until the objective is finite again.
walled <- function(p) if (p[1] > 1.5) NaN else if (p[1] < -1.5) Inf else rosen(p)
show(suppressWarnings(nlminb(c(0, 3), logged(walled, "f"))))
show(suppressWarnings(nlminb(c(0, 3), walled, rosen_gradient)))
fit <- tryCatch(nlminb(c(1.4, 1), function(p) NA), warning = function(w) conditionMessage(w))
print(fit)

# Starting at a bound, with a gradient that points out of the box.
show(nlminb(0, function(x) x[1], lower = 0))
show(nlminb(0, function(x) x[1], function(x) 1, lower = 0))
show(nlminb(c(0, 1), function(x) sum(x), function(x) c(1, 1), lower = c(0, 0), upper = c(5, 5)))
show(nlminb(c(0, 1), function(x) sum(x), lower = c(0, 0), upper = c(5, 5)))

# Controls PORT rejects, and a scale it rejects.
show(nlminb(c(-1.2, 1), rosen, scale = c(1, 0)))
print(tryCatch(nlminb(c(-1.2, 1), rosen, control = list(rel.tol = 1)), error = function(e) conditionMessage(e)))
show(nlminb(c(-1.2, 1), rosen, control = list(rel.tol = 0.5)))

# The final .par holds the solution with the start's names.
fit <- nlminb(c(a = -1.2, b = 1), rosen, rosen_gradient)
print(names(fit$par))
