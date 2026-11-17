# `class<-` names a class by the text of any other vector, and refuses a zero-length value that is
# no character vector. `oldClass<-` and `attr<-` take only text, except that `oldClass<-` removes
# the class for any zero-length value.
show <- function(expr) tryCatch(
    print(expr),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
values <- list(1L, 2.5, TRUE, NA, c(1, 2), list("a"), list(1, "b"), quote(a), as.raw(1), 1i,
    integer(0), logical(0), list(), character(0), NULL)
for (value in values) {
    cat("--", deparse(value), "\n")
    x <- 1
    show({class(x) <- value; x})
    x <- 1
    show({oldClass(x) <- value; x})
    x <- 1
    show({attr(x, "class") <- value; x})
}
x <- 1
show({class(x) <- sum; x})
m <- matrix(1:4, 2)
show({class(m) <- 1:2; attributes(m)})
