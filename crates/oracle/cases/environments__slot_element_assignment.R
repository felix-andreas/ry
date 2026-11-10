# `x[[i]] <- v`, `x[i] <- v` and `x$name <- v` in a function whose frame keeps its variables in
# slots: a local the frame owns changes in place, while a formal, a value another variable shares
# and a value a closure captured are copied first. Each runs a few times so that its call site
# settles on its fastest calling convention.
f <- function(n, i) { x <- numeric(10); l <- list(1, 2, 3); for (k in seq_len(n)) { x[[i]] <- k; x[i + 1L] <- k; l[[2]] <- k; l$a <- k }; list(x, l) }
g <- function(x) { x[[1]] <- 99; x }
y <- c(1, 2, 3)
for (r in 1:3) { str(f(5L, 3L)); print(g(y)); print(y) }
h <- function() { x <- c(1, 2); z <- x; x[[1]] <- 5; c(x, z) }
for (r in 1:3) print(h())
k <- function() { x <- c(1, 2); e <- function() x; x[[2]] <- 7; e() }
for (r in 1:3) print(k())
fm <- function(x, i) { x[[i]] <- 0; x }
v <- c(4, 5, 6); l <- list(1, "a")
for (r in 1:3) { print(fm(v, 2L)); print(v); str(fm(l, 1L)); str(l) }
fs <- function(x) { x[[1]] <- 0; substitute(x) }
for (r in 1:3) print(fs(v))
fe <- function(x) { x[[1]] <- 0; x }
for (r in 1:3) { print(fe(c(7, 8))); print(tryCatch(fe(stop("boom")), error = conditionMessage)) }
fq <- function(x) { x[[2]] <- "z"; x }
for (r in 1:3) print(fq(letters[1:3]))
