# `numToBits` and `numToInts` coerce their argument to double, `rawShift` keeps the names of `x`
# and coerces the shift with GNU R's warnings, and `.colSums` and friends check their dimension
# and `na.rm` arguments. Errors name the calling closure. GNU R 4.6 sums complex matrices.
show <- function(expr) tryCatch(
    withCallingHandlers(print(expr), warning = function(w) {
        cat("warning:", conditionMessage(w), "|", deparse(conditionCall(w)), "\n")
        invokeRestart("muffleWarning")
    }),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
show(numToBits(1L)[1:8])
show(numToInts(1L))
show(numToInts(TRUE))
show(rawShift(c(a = as.raw(1), b = as.raw(2)), 1))
show(rawShift(as.raw(1), "a"))
show(rawShift(as.raw(1), "2"))
show(rawShift(as.raw(1), 1e10))
show(rawShift(1L, 1))
show(rawToBits(1L))
show(.colSums(1:6, 2, 3))
show(.colSums(1:6, NA, 3))
show(.colSums(1:6, -1, 3))
show(.colSums(1:6, 2, 3, NA))
show(.colSums(1:6, 4, 3))
show(.rowMeans(1:6, 2, 3))
show(colSums(matrix(complex(real = 1:4, imaginary = 1), 2)))
show(rowMeans(matrix(complex(real = 1:4, imaginary = c(1, NA, 1, 1)), 2), na.rm = TRUE))
show(colSums(matrix("a", 2, 2)))
