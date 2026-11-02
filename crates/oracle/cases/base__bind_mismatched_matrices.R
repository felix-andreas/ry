# Binding matrices whose bound extents differ is an error naming the argument by its
# position in the call, NULL arguments included.
a <- matrix(1, 4, 2)
b <- matrix(2, 6, 2)
tryCatch(cbind(NULL, a, 1, b), error = function(e) print(conditionMessage(e)))
tryCatch(rbind(t(a), t(b)), error = function(e) print(conditionMessage(e)))
tryCatch(cbind(a, NULL, b), error = function(e) print(conditionMessage(e)))
tryCatch(cbind(matrix(0, 0, 2), a), error = function(e) print(conditionMessage(e)))
print(dim(cbind(matrix(0, 0, 2), 1:3)))
x <- 1:3
print(cbind(x, NULL, y = 4:6, 7:9))
print(rbind(x, NULL, 1:3, deparse.level = 2))
print(dim(cbind(matrix(1, 2, 2), 1:4)))
print(dim(cbind(1:3, 1:6)))
invisible(NULL)
