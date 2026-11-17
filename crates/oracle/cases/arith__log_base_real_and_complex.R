# `log(x, base)` recycles both operands without a length warning, takes the exact `log10` and
# `log2` for those bases, gives `-Inf` at zero and `NaN` with a warning below it, and keeps the
# attributes of the longer operand. A complex operand divides complex logarithms.
show <- function(expr) tryCatch(
    withCallingHandlers(print(expr), warning = function(w) {
        cat("warning:", conditionMessage(w), "\n")
        invokeRestart("muffleWarning")
    }),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
show(log(8, 2))
show(log(1000, 10))
show(log(0, 2))
show(log(0, 3))
show(log(-1, 10))
show(log(-1, 3))
show(log(5, 0))
show(log(5, -1))
show(log(c(a = 1, b = 10, c = 100), 10))
show(log(10, c(x = 10, y = 100)))
show(log(1:4, 2:3))
show(log(NA, NaN))
show(log(NaN, NA))
show(log(numeric(0), 2))
show(log(2, numeric(0)))
show(log("a", 2))
show(log(2, "a"))
show(log(factor(1), 2))
show(log(1 + 1i, 2))
show(log(2, 1i))
show(log(c(NA_complex_, 1i), NA_complex_))
show(log(NA, 1i))
show(log(-1 + 0i, 0))
show(log(c(p = 4 + 0i, q = 8 + 0i), 2))
