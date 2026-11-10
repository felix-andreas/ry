# `x[[y$k]] <- v` and `x[[y[["k"]]]] <- v` compile to tier-0 element assignment; these pin
# evaluation order, dispatch and the cases that must fall back.
f <- function(n) {
  x <- list(a = 1, b = 2); y <- list(k = "a", j = "b", i = 2L)
  for (i in seq_len(n)) {
    x[[y$k]] <- (x[[y$k]] %||% 0) + i
    x[[y[["j"]]]] <- i
    x[y$i] <- list(i * 2)
  }
  x
}
`%||%` <- function(a, b) if (is.null(a)) b else a
str(f(10L)); str(f(10L))
g <- function() { acc <- list(); for (t in list(list(book = "p"), list(book = "q"), list(book = "p"))) acc[[t$book]] <- (acc[[t$book]] %||% 0) + 1; acc }
str(g())
`$.spy` <- function(x, name) { cat("dispatched", name, "\n"); "a" }
h <- function() { x <- list(a = 0); s <- structure(list(), class = "spy"); x[[s$any]] <- 5; x }
str(h())
e <- new.env(); e$k <- "z"
k <- function() { x <- list(); x[[e$k]] <- 1; x }
str(k())
m <- function() { x <- list(a = 1); y <- list(); x[[y$missing]] <- 2; x }
r <- tryCatch(m(), error = function(err) conditionMessage(err)); print(r)
w <- function() { `$` <- function(x, n) "b"; x <- list(a = 1, b = 2); y <- list(k = "a"); x[[y$k]] <- 9; x }
str(w())
