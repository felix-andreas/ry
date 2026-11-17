# `break` and `next` called with arguments are an arity error, and they deparse as the bare
# keyword whatever arguments the call holds.
show <- function(expr) tryCatch(
    print(expr),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
show(`break`(1))
show(`next`(1, 2))
show(deparse(quote(`break`(1))))
show(deparse(quote(`next`(x))))
show(deparse(quote(`break`())))
for (i in 1:3) if (i == 2) `next`() else print(i)
