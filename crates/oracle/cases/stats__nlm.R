# nlm follows the Dennis-Schnabel algorithm, so iteration counts and codes match GNU R;
# numeric results are rounded because difference gradients differ in the last digits.
banana <- function(x) 100 * (x[2] - x[1]^2)^2 + (1 - x[1])^2
fit <- nlm(banana, c(-1.2, 1), hessian = TRUE)
print(round(fit$estimate, 5)); print(fit$code); print(fit$iterations)
print(signif(fit$hessian, 4))
with_gradient <- function(x) {
  structure(banana(x), gradient = c(-400 * x[1] * (x[2] - x[1]^2) - 2 * (1 - x[1]), 200 * (x[2] - x[1]^2)))
}
fit <- nlm(with_gradient, c(-1.2, 1))
print(round(fit$estimate, 6)); print(fit$code); print(fit$iterations)
with_hessian <- function(x) {
  value <- with_gradient(x)
  attr(value, "hessian") <- matrix(c(1200 * x[1]^2 - 400 * x[2] + 2, -400 * x[1], -400 * x[1], 200), 2)
  value
}
fit <- nlm(with_hessian, c(-1.2, 1))
print(round(fit$estimate, 6)); print(fit$code)
shifted <- function(x, target) sum((x - target)^2)
fit <- nlm(shifted, c(10, 10), target = c(3, 5))
print(round(fit$estimate, 6)); print(fit$iterations); print(names(fit))
fit <- nlm(function(x) (x - 2)^4, 10)
print(round(fit$estimate, 2)); print(fit$code); print(fit$iterations)
out <- capture.output(nlm(function(x) sum((x - 1:3)^2), c(0, 0, 0), print.level = 1))
print(grepl("iteration|Parameter|Function Value|Gradient|Relative gradient", out[nzchar(out)][1:4]))
tryCatch(nlm(function(x) "a", 1), error = function(e) print(conditionMessage(e)))
