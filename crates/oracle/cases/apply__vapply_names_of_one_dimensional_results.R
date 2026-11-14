# vapply names the rows of its answer after the first result's names, and a one-dimensional
# array such as a table carries its names as dimnames.
vocabulary <- c("a", "b", "c")
tokens <- list(c("a", "b"), c("c", "c"))
counts <- vapply(tokens, function(words) table(factor(words, levels = vocabulary)), numeric(length(vocabulary)))
str(counts)
str(t(counts))
str(vapply(1:2, function(v) array(v, 2, list(c("p", "q"))), numeric(2)))
str(vapply(c(x = 1, y = 2), function(v) array(v, 2, list(c("p", "q"))), numeric(2)))
str(vapply(1:2, function(v) array(v, 2, list(c("p", "q"))), numeric(2), USE.NAMES = FALSE))
str(vapply(1:2, function(v) array(v, 2, list(c("p", "q"))), c(m = 0, n = 0)))
str(vapply(1:2, function(v) c(p = v, q = v), numeric(2)))
str(vapply(1:2, function(v) array(v, 1, list("p")), numeric(1)))
str(vapply(1:2, function(v) matrix(v, 1, 2, dimnames = list("r", c("p", "q"))), matrix(0, 1, 2)))
str(vapply(1:2, function(v) matrix(v, 1, 2, dimnames = list("r", c("p", "q"))), numeric(2)))
# A zero-length input still takes its row names from FUN.VALUE.
str(vapply(integer(0), function(i) c(a = 1, b = 2), c(a = 0, b = 0)))
str(vapply(character(0), function(i) table(c("x", "y")), table(c("x", "y"))))
str(vapply(integer(0), function(i) 1, numeric(1)))
