# `.C` and `.Fortran` refuse missing and non-finite arguments unless `NAOK = TRUE`, for the
# default packages' routines too, and the error names the calling closure.
f <- function() .Fortran(stats:::C_setppr, 0, 0, 2L, 0L, NaN, 1)
tryCatch(f(), error = function(e) print(e))
tryCatch(.Fortran(stats:::C_setppr, 0, 0, NA_integer_, 0L, 1, 1), error = function(e) print(conditionMessage(e)))
str(.Fortran(stats:::C_setppr, 0, 0, 2L, 0L, NaN, 1, NAOK = TRUE)[[5]])
d <- data.frame(y = c(1, 2, 3, 4, 5, 6), x = c(1, Inf, 3, 4, 5, 6))
tryCatch(ppr(y ~ x, data = d, nterms = 1), error = function(e) print(conditionMessage(e)))

# A seasonal order too large for starma's `int` sizes is its fault 7, not an allocation.
set.seed(1)
tryCatch(arima0(rnorm(200), seasonal = list(order = c(1, 0, 0), period = 3000)), error = function(e) print(e))

