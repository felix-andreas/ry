# The two-argument math functions (`choose`, `beta`, `lbeta`, `lchoose`, `atan2`) recycle
# without a length warning, give `NA` when either operand is `NA` even against `NaN`, warn when
# they make a `NaN`, keep the attributes of the longer operand, and refuse factors and text.
show <- function(expr) tryCatch(
    withCallingHandlers(print(expr), warning = function(w) {
        cat("warning:", conditionMessage(w), "|", deparse(conditionCall(w)), "\n")
        invokeRestart("muffleWarning")
    }),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
show(choose(1:5, 2:3))
show(choose(c(a = 5, b = 6), 2))
show(choose(5, c(a = 2, b = 3, c = 1)))
show(choose(NaN, NA))
show(choose(NA, NaN))
show(beta(NaN, NA_integer_))
show(beta(-1, 1))
show(lbeta(-1, 2))
show(lchoose(-1.5, 2))
show(beta(numeric(0), 1))
show(beta(1, numeric(0)))
show(beta(structure(numeric(0), foo = "x"), 1))
show(beta(matrix(1:4, 2), 1:3))
show(beta(factor(1), 1))
show(choose("a", 1))
show(atan2(1:3, 1:2))
show(atan2(c(a = 1, b = 2), 1))
show(atan2(matrix(1:4, 2), 1:2))
show(atan2("a", 1))
show(atan2(1i, "a"))
