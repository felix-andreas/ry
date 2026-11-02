# A numeric index matrix drops a row containing 0 and gives NA for a row containing NA.
m <- matrix(1:6, ncol = 2)
m[matrix(c(1, 1, 1, 0, 1, NA), ncol = 2, byrow = TRUE)]
m[matrix(c(1, 0, 1, 2), ncol = 2, byrow = TRUE)]
tryCatch(m[matrix(c(-1, 1), ncol = 2)], error = conditionMessage)

# seq.int with length.out returns integers when the start and step are whole.
str(seq.int(0, 1, length.out = 1))
str(seq.int(2, 10, length.out = 3))
str(seq.int(1L, by = 2, length.out = 3))
str(seq.int(0, 1, length.out = 4))
str(seq.int(1, 10, 3))
str(seq.int(1L, 10L, 3L))
topo.colors(5)

# A model frame for a formula without variables keeps the data's rows.
nrow(model.frame(~1, data = data.frame(x = 1:4)))

# S4 methods for group generics reached through internal dispatch see .Generic.
setClass("Box", representation(x = "numeric"))
setMethod("Summary", "Box", function(x, ..., na.rm = FALSE) {
  paste(.Generic, do.call(.Generic, list(x@x)))
})
b <- new("Box", x = c(3, 1, 2))
max(b)
min(b)

# A logical subscript longer than its dimension is an error; a shorter one recycles.
m <- matrix(1:36, 6)
long_rows <- c(TRUE, rep(FALSE, 8))
print(tryCatch(m[long_rows, ], error = conditionMessage))
print(tryCatch(m[, long_rows, drop = FALSE], error = conditionMessage))
print(m[c(TRUE, FALSE), ])
