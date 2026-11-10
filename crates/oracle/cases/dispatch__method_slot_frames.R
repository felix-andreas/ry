# S3 methods keep their dispatch variables beside the frame until something asks for the
# environment; NextMethod, reads of `.Generic`, and `environment()` must all see them.
p <- function(x, ...) UseMethod("p")
p.default <- function(x, ...) paste("default", unclass(x))
p.a <- function(x, ...) paste("a", NextMethod())
p.b <- function(x, ...) { r <- NextMethod(); paste("b", r) }
p.c <- function(x, ...) paste("c", .Generic, paste(.Class, collapse = "/"))
p.d <- function(x, k = 2, ...) {
  e <- environment()
  paste("d", exists(".Generic", envir = e, inherits = FALSE), k)
}
p.e <- function(x, ...) { f <- function() .Generic; paste("e", f()) }
p.g <- function(x, y = x) { x <- 5; paste("g", y, NextMethod()) }
Ops.q <- function(e1, e2) {
  v <- get(.Generic)(unclass(e1), unclass(e2))
  paste(.Generic, v)
}
for (i in 1:60) {
  r <- c(p(structure(1, class = c("b", "a"))), p(structure(1, class = "c")),
         p(structure(1, class = "d")), p(structure(1, class = "e")),
         p(structure(1, class = c("g", "a"))), structure(2, class = "q") + 3)
}
cat(r, sep = "\n")
