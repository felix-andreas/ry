# nlm, integrate and findInterval run the same UNCMIN, QUADPACK and interval search as the
# C API's optif9, Rdqags/Rdqagi and findInterval2, and match GNU R to the last bit.
options(digits = 15)
tc <- function(e) tryCatch(e, error = function(e) conditionMessage(e))
show <- function(fit) {
  if (is.character(fit)) return(print(fit))
  print(c(fit$minimum, fit$estimate, fit$gradient)); cat(fit$code, fit$iterations, "\n")
}
calls <- 0
capped <- function(x) { calls <<- calls + 1; if (x[1] > 3) Inf else sum((x - c(5, 1))^2) }
show(suppressWarnings(nlm(capped, c(0, 0))))
cat("calls", calls, "\n")
banana <- function(x) 100 * (x[2] - x[1]^2)^2 + (1 - x[1])^2
gradient <- function(x) structure(banana(x), gradient = c(-400 * x[1] * (x[2] - x[1]^2) - 2 * (1 - x[1]), 200 * (x[2] - x[1]^2)))
wrong_hessian <- function(x) { value <- gradient(x); attr(value, "hessian") <- diag(2) * 5; value }
show(tc(nlm(wrong_hessian, c(-1.2, 1))))
show(tc(nlm(wrong_hessian, c(-1.2, 1), check.analyticals = FALSE)))
show(tc(nlm(function(x) structure(banana(x), gradient = c(1, 1)), c(-1.2, 1))))
show(tc(nlm(banana, c(-1.2, 1), ndigit = 0)))
show(tc(nlm(banana, c(-1.2, 1), typsize = c(10, 0.1))))
show(tc(nlm(function(x) (x - 2)^4 + x^2, 10)))
show(tc(nlm(banana, numeric(0))))
nlm(function(x) sum((x - 1:2)^2) + x[1]^4, c(0, 0), print.level = 2)$code

# The objective's call protocol: a probe at the start whose value is ignored, derivatives
# accepted only when the probe supplies them with the right length and mode, a memory of five
# evaluations, and an error instead of a call at a non-finite point.
counted <- function(at, change, start = c(0, 0), hessian = NULL, iterlim = 2) {
  calls <<- 0
  objective <- function(x) {
    calls <<- calls + 1
    value <- sum((x - 1)^2)
    attr(value, "gradient") <- 2 * (x - 1)
    attr(value, "hessian") <- hessian
    if (calls == at) change(value) else value
  }
  fit <- tc(withCallingHandlers(nlm(objective, start, iterlim = iterlim),
    warning = function(w) { cat("warning:", conditionMessage(w), "\n"); invokeRestart("muffleWarning") }))
  cat("calls", calls, ""); show(fit)
}
for (at in 1:3) {
  counted(at, function(value) c(1, 2))
  counted(at, function(value) NaN)
  counted(at, function(value) Inf * value)
  counted(at, function(value) structure(value, gradient = 1:3))
  counted(at, function(value) structure(value, gradient = c("a", "b")))
}
counted(1, function(value) structure(value, gradient = NULL, hessian = diag(2)))
counted(1, function(value) structure(value, hessian = 1:3), hessian = diag(2) * 2)
counted(0, identity, hessian = c(2L, 0L, 0L, 2L))
for (d in 4:6) counted(0, identity, start = rep(0, d), hessian = diag(d) * 2, iterlim = 1)
print(tc(nlm(function(x) sum(x^2), c(Inf, 0))))

quad <- function(r) if (is.character(r)) print(r) else print(c(r$value, r$abs.error, r$subdivisions))
quad(integrate(dnorm, -Inf, Inf))
quad(integrate(dnorm, -Inf, 1.5))
quad(integrate(function(x) 1/((x + 1) * sqrt(x)), 0, Inf))
quad(integrate(function(x) log(x), 0, 1))
quad(integrate(function(x) x^-0.9, 0, 1))
quad(integrate(function(x) cos(100 * x), 0, pi))
quad(tc(integrate(function(x) sin(1/x), 0.001, 1, subdivisions = 5)))
quad(tc(integrate(function(x) 1/x, 0, 1)))
quad(tc(integrate(function(x) rep(1, length(x)), 0, 1, rel.tol = 1e-20, abs.tol = 0)))

breaks <- c(1, 2, 2, 3, 5)
points <- c(-1, 1, 1.5, 2, 2.5, 3, 5, 6, NA)
for (closed in c(FALSE, TRUE)) for (inside in c(FALSE, TRUE)) for (open in c(FALSE, TRUE))
  print(findInterval(points, breaks, rightmost.closed = closed, all.inside = inside, left.open = open))
print(findInterval(points, numeric(0)))
print(findInterval(points, 2, all.inside = TRUE))
