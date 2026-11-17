# `polyroot` drops trailing zero coefficients first, so an `NA` constant of a polynomial of
# degree 0 gives no roots, while a missing coefficient of a higher degree is an error. Text and
# lists are unimplemented types.
show <- function(expr) tryCatch(
    print(expr),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
show(polyroot(c(1, NA)))
show(polyroot(c(NA, 0)))
show(polyroot(NA))
show(polyroot(c(1, 2, NA, 0)))
show(polyroot(c(6, -5, 1, 0, 0)))
show(polyroot(c(0, 0)))
show(polyroot("a"))
show(polyroot(list(1)))
