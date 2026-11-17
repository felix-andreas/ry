# A promise whose code left through an error warns when it is forced again.
show <- function(expr) withCallingHandlers(
    tryCatch(expr, error = function(e) paste("error:", conditionMessage(e))),
    warning = function(w) {
        cat("warning:", conditionMessage(w), "\n")
        print(conditionCall(w))
        invokeRestart("muffleWarning")
    }
)
f <- function(p) list(show(p), show(p), show(p))
f(stop("bad argument"))

# The value of a later successful force is kept, and forcing again is silent.
count <- 0
g <- function(p) list(show(p), show(p), show(p))
g({ count <- count + 1; if (count < 2) stop("first time") else count })

# A default argument and a jump other than an error.
h <- function(p = stop("default failed")) c(show(p), show(p))
h()
k <- function(p) {
    first <- tryCatch(p, note = function(c) "jumped out")
    list(first, show(p))
}
k({ signalCondition(structure(class = c("note", "condition"), list(message = "note", call = NULL))); "finished" })

# A handler that turns the warning into a jump leaves the promise under evaluation.
m <- function(p) {
    show(p)
    one <- tryCatch(p, warning = function(w) "caught the warning")
    two <- show(p)
    list(one, two)
}
m(stop("bad argument"))

# delayedAssign promises follow the same rule.
delayedAssign("lazy", stop("lazy failed"))
show(lazy)
show(lazy)

# An argument passed on is a promise of its own, so each one on the chain warns.
inner <- function(y) list(show(y), show(y))
outer <- function(x) inner(x)
outer(stop("passed on"))

# An argument that fails in one call leaves no mark on the arguments of later calls.
once <- function(p) show(p)
once(stop("first call"))
once(sqrt(4))
once(stop("third call"))
