# `[[` and `.subset2` called without any index are an error before the object is looked at,
# while an empty index written inside the brackets is a missing subscript.
show <- function(expr) tryCatch(
    print(expr),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
l <- list(1)
show(`[[`(1))
show(.subset2(1))
show(.subset2(l))
show(.subset2(NULL))
show(l[[exact = TRUE]])
show(.subset2(1, exact = FALSE))
show(l[[]])
