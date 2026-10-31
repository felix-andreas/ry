x <- structure(list(a = 1:3), class = "box")
as.character.box <- function(x, ...) "a box"
as.character(x)
rev.box <- function(x) "reversed box"
rev(x)
unique(c(3, 1, 3, 2))
seq_along(x)
methods_found <- exists("print.box")
methods_found
summary(c(1, 2, 3, 4, 100))
t.test2 <- function(x) UseMethod("t.test2")
tryCatch(t.test2(1), error = function(e) conditionMessage(e))
