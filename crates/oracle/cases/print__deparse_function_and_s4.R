# A function without source deparses its formals and body without the attributes of constants,
# which is how a generic's skeleton call prints.
f <- function() NULL
body(f) <- call("g", structure("a", package = "p"))
cat(deparse(f), sep = "\n")
cat(deparse(as.call(list(f, 1))), sep = "\n")
formals(f) <- alist(x = )
formals(f)$x <- structure(1, unit = "cm")
cat(deparse(f), sep = "\n")
cat(deparse(call("g", structure("a", package = "p"))), sep = "\n")
setClass("Wide", representation(alpha = "character", beta = "character", gamma = "numeric"))
w <- new("Wide", alpha = "a long enough value", beta = "another long value", gamma = 1)
cat(deparse(w), sep = "\n")
cat(deparse(list(w, w)), sep = "\n")
