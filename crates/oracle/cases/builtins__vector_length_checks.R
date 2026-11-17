# `vector()` and its typed shortcuts check the length: text is coerced with a warning, and NA,
# infinite and negative lengths are errors, as is a mode that is not one string. `.row` and
# `.col` take only a dimension vector of length 2.
show <- function(expr) tryCatch(
    withCallingHandlers(print(expr), warning = function(w) {
        cat("warning:", conditionMessage(w), "|", deparse(conditionCall(w)), "\n")
        invokeRestart("muffleWarning")
    }),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
show(integer("a"))
show(numeric("2"))
show(vector("list", "a"))
show(vector(1:3, "a"))
show(vector(1:3, 2))
show(vector("numeric", 1:2))
show(single(Inf))
show(single(length = TRUE))
show(logical(-1))
show(vector("foo", 1))
show(vector(c("list", "integer"), 1))
show(.row(1:3))
show(.col(1:3))
show(.row(c(2, 2)))
