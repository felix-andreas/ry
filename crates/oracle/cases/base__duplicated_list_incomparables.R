# `duplicated` on a list never marks an element equal to one of `incomparables`, compared as
# `identical` values after coercion to the type of `x`.
show <- function(expr) tryCatch(
    print(expr),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
show(duplicated(list(1, 1, 2), list(1), 1))
show(duplicated(list(1, 1, 2), list(1)))
show(duplicated(list(1, 1, 2, 2), 2))
show(duplicated(list("a", "a", 1, 1), "a"))
show(duplicated(c(1, 1, 2), list(1)))
show(duplicated(c(1, 1, 2, 2), 2))
show(unique(list(1, 1, 2, 2), incomparables = list(2)))
