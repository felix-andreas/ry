# An S4 object without a `[[` method of its own falls back to the primitive after the generic
# finds no method, and the primitive still sees the argument values.
setClass("A", representation(n = "character"))
setMethod("[[", "A", function(x, i, ...) "A")
setClass("C", contains = "numeric")
f <- new("C", c(1, 2))
i <- 2
print(f[[1]])
print(f[[i]])
print(new("A", n = "z")[[1]])
foo <- setClass("foo", slots = list(a = "integer"))
print(tryCatch(foo()[[1]], error = conditionMessage))
