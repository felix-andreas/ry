# The binding queries require a symbol and an environment, and `new.env`, `list2env` and
# `as.list.environment` check their arguments in the order GNU R does.
show <- function(expr) tryCatch(
    withCallingHandlers(print(expr), warning = function(w) {
        cat("warning:", conditionMessage(w), "|", deparse(conditionCall(w)), "\n")
        invokeRestart("muffleWarning")
    }),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
e <- new.env()
assign("a", 1, e)
show(bindingIsLocked(1, e))
show(bindingIsLocked("zz", e))
show(bindingIsActive(1, e))
show(bindingIsLocked(c("a", "b"), e))
show(bindingIsLocked("a", c(2, 1)))
show(bindingIsLocked(quote(a), e))
show(environmentIsLocked(1))
show(environmentIsLocked(NULL))
show(class(new.env(Inf)))
show(new.env(1, 2))
show(new.env(TRUE, quote(v + 1), 1))
show(list2env(list(a = 1), 1, 0.5))
show(list2env(list(a = 1), NULL, 5, hash = FALSE))
show(as.list.environment(1, "sum"))
show(makeActiveBinding(1, function() 1, e))
