# The odd elementary functions answer a negative zero with a positive zero, as GNU R 4.6 does,
# while `sqrt` keeps the sign.
show <- function(expr) tryCatch(
    print(expr),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
show(1 / atan(-0))
show(1 / sin(-0))
show(1 / tan(-0))
show(1 / asin(-0))
show(1 / atanh(-0))
show(1 / sinh(-0))
show(1 / tanh(-0))
show(1 / asinh(-0))
show(1 / expm1(-0))
show(1 / log1p(-0))
show(1 / sqrt(-0))
show(1 / atan(c(-0, 0, 1)))
f <- function(y) 1 / sin(y)
show(f(-0))
