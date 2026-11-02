# Complex assignment: the value promise substitute() sees, the generated `*tmp*` calls that errors name, and the errors of `[[<-` and `[<-`.
`[[<-.foo` <- function(x, i, value) { print(substitute(value)); print(sys.call()); x }
x <- structure(list(), class = "foo")
y <- 3
x[["a"]] <- y + 1
`$<-.foo` <- function(x, name, value) { print(substitute(value)); print(sys.call()); x }
x$b <- y * 2
`f<-` <- function(x, value) { print(substitute(value)); print(sys.call()); x }
z <- 1
f(z) <- y - 1
l <- list(a = 1)
l$a$b <- 1
r <- tryCatch({ x <- 1:3; x[1:2] <- 1:3 }, warning = function(w) conditionCall(w))
print(r)
r <- tryCatch({ m <- matrix(1:4, 2); m[3, 1] <- 1 }, error = function(e) conditionCall(e))
print(r)
for (e in c(quote(x[[1:2]] <- 1), quote(x[[NA]] <- 1), quote(x[[4]] <- NULL), quote(x[1] <- sum))) {
  x <- 1:3
  r <- tryCatch(eval(e), error = function(e) e)
  print(r)
}
x <- list(a = 1:3)
r <- tryCatch(x$a[["b"]][[5]] <- 1:2, error = function(e) e)
print(r)
e <- new.env(); assign("v", 1:3, e)
r <- tryCatch(e$v[[1:3]] <- 1, error = function(e) e)
print(r)
k <- list(a = 1); r <- tryCatch(k[["b"]][["c"]] <- 1, error = function(e) e); print(class(r))
show <- function(expr) {
  r <- tryCatch(withCallingHandlers(expr, warning = function(w) { cat("W:", deparse(conditionCall(w)), "|", conditionMessage(w), "\n"); invokeRestart("muffleWarning") }),
                error = function(e) cat("E:", deparse(conditionCall(e)), "|", conditionMessage(e), "\n"))
  invisible()
}
f <- function() {
  x <- 1:3; m <- matrix(1:4, 2); l <- list(a = 1, b = 2); e <- new.env()
  show(x[1:2] <- 1:3)
  show(m[3, 1] <- 1)
  show(`[<-`(m, 3, 1, value = 1))
  show(x[[1:2]] <- 1)
  show(`[[<-`(x, 1:2, value = 1))
  show(x[[NA]] <- 1)
  show(x[[4]] <- NULL)
  show(x[[4]] <- integer())
  show(x[1] <- sum)
  show(x[1:2] <- integer())
  show(x[[0]] <- 1)
  show(x[[-1]] <- 1)
  show(l[[3:4]] <- 1)
  show(l[[c("a", "b")]] <- 1)
  show(m[[3, 1]] <- 1)
  show(m[1, 2, 3] <- 1)
  show(x[["a"]][["b"]] <- 1)
  show(l$a[[5]][[2]] <- 1)
  show(x$a <- 1)
  show(m[1, ] <- 1:3)
  show(x[list()] <- 1)
  show(x[-1:1] <- 1)
  show(names(x)[2:5] <- letters)
  show(levels(x) <- 1)
  show(dim(x) <- 2)
  show(attr(x, 1) <- 2)
  show(x@a <- 1)
  show(e[[1]] <- 1)
  show(e[1] <- 1)
  show(x[[TRUE]] <- 1:2)
  show(l[[c(1, 5)]] <- 1)
}
f()
# Replacement type rules of `[<-` and `[[<-`.
xs <- list(lgl = TRUE, int = 1L, dbl = 1, cplx = 1i, chr = "a", raw = as.raw(1), list = list(1), expr = expression(1))
ys <- list(lgl = NA, int = 2L, dbl = 2, cplx = 2i, chr = "b", raw = as.raw(2), list = list(2), expr = expression(2), closure = function() 1, builtin = sum, special = `if`, symbol = quote(s), lang = quote(a + b), env = globalenv())
for (xn in names(xs)) for (yn in names(ys)) {
  x <- xs[[xn]]
  r1 <- tryCatch({ x[1] <- ys[[yn]]; typeof(x) }, error = function(e) conditionMessage(e))
  x <- xs[[xn]]
  r2 <- tryCatch({ x[[1]] <- ys[[yn]]; typeof(x) }, error = function(e) conditionMessage(e))
  cat(xn, yn, "|", r1, "|", r2, "\n")
}
# `[[<-` with one subscript, by target, subscript and value.
objects <- list(int = 1:3, named = c(a = 1, b = 2), two = c(5L, 6L), list = list(1, 2, 3), null = NULL, chr = c("p", "q"))
indices <- list(0L, 0, -1L, -1, -2, NA, NA_integer_, NA_real_, 1:2, integer(), "a", "z", 4, 2.7, TRUE, FALSE, c(TRUE, FALSE), Inf, -Inf, list(), NULL, "")
vals <- list(1, NULL, integer(), 1:2, sum, list(1), "s", NA)
for (o in names(objects)) for (i in seq_along(indices)) for (v in seq_along(vals)) {
  x <- objects[[o]]
  r <- tryCatch({ x[[indices[[i]]]] <- vals[[v]]; paste(deparse(x), collapse = "") },
                error = function(e) paste("E:", conditionMessage(e)),
                warning = function(w) paste("W:", conditionMessage(w)))
  cat(o, deparse(indices[[i]]), deparse(vals[[v]])[1], "=>", r, "\n")
}
