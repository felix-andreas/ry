# `storage.mode<-` takes one non-NA string naming a type, where `real` and `single` are defunct,
# and keeps the attributes of the value it converts.
show <- function(expr) tryCatch(
    print(expr),
    error = function(e) cat("error:", conditionMessage(e), "|", deparse(conditionCall(e)), "\n")
)
x <- 1
show({ storage.mode(x) <- FALSE; x })
show(`storage.mode<-`(1, NA_character_))
show(`storage.mode<-`(1, character()))
show(`storage.mode<-`(1, "real"))
show(`storage.mode<-`(1, "single"))
show(`storage.mode<-`(1, "foo"))
show(`storage.mode<-`(c(a = 1.5), "integer"))
show(`storage.mode<-`(matrix(1:4, 2), "double"))
show(`storage.mode<-`(factor("a"), "character"))
