# A scalar argument that a builtin coerces warns as GNU R's `asInteger` and `asReal` do: text that
# is no number gives `NA` with a warning, a double outside the integer range gives `NA` with
# its own warning, and a complex value drops its imaginary part with a warning.
show <- function(expr) tryCatch(
    withCallingHandlers(print(expr), warning = function(w) {
        cat("warning:", conditionMessage(w), "\n")
        invokeRestart("muffleWarning")
    }),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
show(match(1:3, 2, nomatch = "a"))
show(match(1:3, 2, nomatch = 1e10))
show(match(1:3, 2, nomatch = "7"))
show(match(1:3, 2, nomatch = 1 + 1i))
show(match(1:3, 2, nomatch = 2.7))
show(seq_len("a"))
show(seq_len("3"))
show(seq_len(2 + 1i))
show(crossprod(1:2, 1:2, 3))
show(tcrossprod(1:2, 1:2, 3))
