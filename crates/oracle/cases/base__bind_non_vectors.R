# `cbind` and `rbind` make a list matrix when an argument is no vector: a call binds its
# elements and a symbol or function is one element. An expression vector is refused.
show <- function(expr) tryCatch(
    print(expr),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
show(cbind(quote(a)))
show(rbind(1, quote(f(x))))
show(cbind(sum, 1:2))
show(typeof(cbind(quote(a), 1)))
show(dim(rbind(quote(f(x, y)), 1:3)))
show(cbind(expression(a), 1))
