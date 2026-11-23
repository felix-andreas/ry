# Arithmetic and comparison in a call the optimizing tier runs without a frame work on
# strings and on vectors with attributes, and dispatch on an object's class from the real
# frame: an S3 method, an S4 method, and a group generic that reads its call.

weight <- function(kind) if (kind == "a") 1 else if (kind == "b") 2 else 3
scale <- function(x, k) x * k + 1
total <- function(kinds) {
  sum <- 0
  for (kind in kinds) sum <- sum + weight(kind)
  sum
}
scaled <- function(x, k) scale(x, k)

for (k in 1:40) {
  weight("a"); scale(2, 3)
}
for (k in 1:40) {
  total(c("a", "b", "c")); scaled(2, 3)
}
print(total(c("a", "b", "c", "a")))
print(scaled(2, 3))
print(scaled(c(a = 1, b = 2), 3))
print(scaled(matrix(1:4, 2), 2L))
print(tryCatch(scaled(as.Date("2024-01-02"), 1), error = function(e) conditionMessage(e)))
print(scaled(as.difftime(2, units = "mins"), 3))
print(tryCatch(scaled("a", 2), error = function(e) conditionMessage(e)))
print(tryCatch(total(list("a", NULL)), error = function(e) conditionMessage(e)))
print(total(factor(c("a", "b"))))

"==.tagged" <- function(e1, e2) {
  cat("comparing from", deparse(sys.call(-1)), "\n")
  unclass(e1) == e2
}
"*.tagged" <- function(e1, e2) {
  cat("frames:", sys.nframe(), "\n")
  structure(unclass(e1) * e2, class = "tagged")
}
print(weight(structure("b", class = "tagged")))
print(total(list("a", structure("b", class = "tagged"))))
print(unclass(scaled(structure(2, class = "tagged"), 3)))

setClass("Amount", representation(value = "numeric"))
setMethod("*", signature("Amount", "numeric"), function(e1, e2) {
  cat("method for", class(e1), "\n")
  e1@value * e2
})
setMethod("Ops", signature("Amount", "character"), function(e1, e2) {
  cat("group generic", .Generic, "\n")
  FALSE
})
print(scaled(new("Amount", value = 2), 3))
print(weight(new("Amount", value = 2)))
print(scaled(2, 3))
print(total(c("c", "b")))
