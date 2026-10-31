setClass("Vec", representation(x = "numeric", y = "numeric"))
v <- new("Vec", x = c(1, 2, 3), y = c(4, 5, 6))
setMethod("+", signature("Vec", "Vec"), function(e1, e2) {
  new("Vec", x = e1@x + e2@x, y = e1@y + e2@y)
})
(v + v)@x
setMethod("length", "Vec", function(x) length(x@x))
length(v)
setMethod("[", "Vec", function(x, i, ...) new("Vec", x = x@x[i], y = x@y[i]))
v[2:3]@y
setMethod("show", "Vec", function(object) cat("<Vec of length", length(object), ">\n"))
v
print(v)
list(v, 1)
capture.output(v)
setClass("Money", representation(amount = "numeric", currency = "character"))
setMethod("Arith", signature("Money", "Money"), function(e1, e2) {
  new("Money", amount = callGeneric(e1@amount, e2@amount), currency = e1@currency)
})
setMethod("Arith", signature("Money", "numeric"), function(e1, e2) {
  new("Money", amount = callGeneric(e1@amount, e2), currency = e1@currency)
})
setMethod("show", "Money", function(object) {
  cat(format(object@amount, nsmall = 2), object@currency, "\n")
})
m <- new("Money", amount = 10, currency = "EUR")
m + m
m * 3
m - m
m / 4
setMethod("Compare", signature("Money", "Money"), function(e1, e2) {
  callGeneric(e1@amount, e2@amount)
})
m < m * 2
m == m
setMethod("Math", "Money", function(x) {
  new("Money", amount = callGeneric(x@amount), currency = x@currency)
})
sqrt(m * 10)
setClass("M", representation(d = "matrix"))
mm <- new("M", d = matrix(1:6, 2))
setMethod("[", signature(x = "M", i = "numeric", j = "missing"),
  function(x, i, j, ..., drop = TRUE) x@d[i, , drop = drop])
setMethod("[", signature(x = "M", i = "missing", j = "numeric"),
  function(x, i, j, ..., drop = TRUE) x@d[, j, drop = drop])
setMethod("[", signature(x = "M", i = "numeric", j = "numeric"),
  function(x, i, j, ..., drop = TRUE) x@d[i, j])
mm[1, ]
mm[, 2]
mm[2, 3]
setClass("Thing", representation(name = "character"))
setMethod("$", "Thing", function(x, name) paste("field", name))
new("Thing", name = "t")$anything
setClass("Num2", contains = "numeric")
n2 <- new("Num2", c(1.5, 2.5))
n2 * 2
sum(n2)
setMethod("[", "Num2", function(x, i, ...) {
  cat("subsetting Num2\n")
  callNextMethod()
})
n2[2]
