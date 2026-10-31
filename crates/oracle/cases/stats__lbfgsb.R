fr <- function(x) 100 * (x[2] - x[1]^2)^2 + (1 - x[1])^2
grr <- function(x) c(-400 * x[1] * (x[2] - x[1]^2) - 2 * (1 - x[1]), 200 * (x[2] - x[1]^2))
o <- optim(c(-1.2, 1), fr, method = "L-BFGS-B", control = list(trace = 1)); print(o)
print(optim(c(-1.2, 1), fr, grr, method = "L-BFGS-B"))
print(optim(c(-1.2, 1), fr, method = "L-BFGS-B", lower = c(-2, -2), upper = c(0.5, 2)))
flb <- function(x) { p <- length(x); sum(c(1, rep(4, p - 1)) * (x - c(1, x[-p])^2)^2) }
print(optim(rep(3, 25), flb, NULL, method = "L-BFGS-B", lower = rep(2, 25), upper = rep(4, 25)))
o <- optim(c(1, 1), function(p) sum((p - c(3, -2))^2), method = "L-BFGS-B", lower = 0); print(o$par); print(o$counts); print(o$message)
print(optim(c(5, 5, 5), function(p) sum((p - 1:3)^4) + prod(p), method = "L-BFGS-B", lower = -1, upper = 4, hessian = TRUE))
hw <- HoltWinters(co2)
print(c(hw$alpha, hw$beta, hw$gamma, hw$SSE))
print(StructTS(Nile, "level"))
print(StructTS(log10(UKgas), type = "BSM"))
