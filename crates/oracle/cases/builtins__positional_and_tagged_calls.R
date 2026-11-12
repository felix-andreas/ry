# A builtin call site whose arguments are all untagged and none empty runs a form that takes the
# values straight from the registers; a tagged or empty argument keeps the general call. Each
# function loops so that it is compiled, and runs twice so that the second run sees the site after
# its first call. These pin that both forms answer as GNU R does.
twice <- function(f) for (run in 1:2) print(tryCatch(f(), error = conditionMessage))

twice(function() { for (i in 1:2) r <- c(1, 2, 3); r })
twice(function() { for (i in 1:2) r <- c(a = 1, 2, b = 3); r })
twice(function() { for (i in 1:2) r <- c(1, , 3); r })
twice(function() { for (i in 1:2) r <- length(1:3, 4); r })
twice(function() { for (i in 1:2) r <- length(x = 1:3); r })
twice(function() { for (i in 1:2) r <- max(4, NA, 2, na.rm = TRUE); r })
twice(function() { for (i in 1:2) r <- rep_len(length.out = 5, x = 1:2); r })
twice(function() { for (i in 1:2) r <- seq_len(3); r })
twice(function() { m <- matrix(1:6, 2); for (i in 1:2) r <- m[, 2]; r })
twice(function() { m <- matrix(1:6, 2); for (i in 1:2) r <- m[2, ]; r })
twice(function() { x <- list(a = 1, b = "z"); for (i in 1:2) r <- x[["b"]]; r })
twice(function() { y <- 1:2; for (i in 1:2) z <- `names<-`(y, c("p", "q")); list(y, z) })
twice(function() { for (i in 1:2) r <- nchar("abcd"); r })
twice(function() { for (i in 1:2) r <- nchar("abcd", type = "bytes"); r })
twice(function() { for (i in 1:2) r <- paste0("a", 1:2, collapse = "+"); r })

# A site that answered through one form keeps answering when its operands change kind.
g <- function(x) { for (i in 1:2) r <- c(x, x); r }
print(g(1L)); print(g("s")); print(g(list(1))); print(g(NULL))
h <- function(x) { for (i in 1:2) r <- length(x); r }
print(h(1:4)); print(h(structure(list(), class = "thing")))
length.thing <- function(x) 99L
print(h(structure(list(), class = "thing")))
