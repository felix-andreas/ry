# A raw vector compares with a complex one after both become complex: equality and inequality
# work, NA stays NA, and an ordering comparison is an error.
show <- function(expr) tryCatch(
    print(expr),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
show(c(complex(real = Inf, imaginary = -0), NA_complex_) != as.raw(c(0, 255)))
show(1i != as.raw(1))
show(1i == as.raw(0))
show(as.raw(1) == 1 + 0i)
show(as.raw(c(1, 2)) == c(1 + 0i, 2 + 1i))
show(as.raw(1) < 2L)
show(as.raw(1) < 1i)
