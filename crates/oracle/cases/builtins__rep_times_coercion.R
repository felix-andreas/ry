# `rep` coerces a `times` that is not a double to integer, and a double count is invalid only
# when it is NaN or at most -1.
show <- function(x) withCallingHandlers(
    tryCatch(x, error = function(e) paste("E:", conditionMessage(e))),
    warning = function(w) {
        cat("W:", conditionMessage(w), "\n")
        invokeRestart("muffleWarning")
    }
)
print(show(rep(1, -0.5)))
print(show(rep(1:2, c(-0.5, 1))))
print(show(rep(1, -1)))
print(show(rep(1:2, c(-1, 1))))
print(show(rep(1, "2")))
print(show(rep(1, "NA")))
print(show(rep(1, "2.9")))
print(show(rep(1:2, c("1", "x"))))
print(show(rep(1:2, c("1", "2"))))
print(show(rep(1, list(2))))
print(show(rep(1:2, list(1, 2))))
print(show(rep(1, TRUE)))
print(show(rep(1, NA)))
print(show(rep(1:2, c(NA, 1))))
print(show(rep(1, 2+0i)))
print(show(rep(1, 2+1i)))
print(show(rep(1, as.raw(2))))
print(show(rep(1:2, c(1.9, 2.9))))
print(show(rep(1, 3e9)))
print(show(rep(1:2, NULL)))
print(show(rep(1:2, integer(0))))
