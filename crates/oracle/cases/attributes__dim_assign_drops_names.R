# `dim<-` drops the names whatever the new dims are, `NULL` included, where `attr(x, "dim") <-`
# keeps them; both drop the dimnames, and a pairlist's tags survive `dim(x) <- NULL`.
x <- c(a = 1, b = 2); dim(x) <- NULL; print(attributes(x))
x <- c(a = 1, b = 2); dim(x) <- 2; print(attributes(x))
x <- c(a = 1, b = 2); attr(x, "dim") <- 2L; print(attributes(x))
x <- c(a = 1, b = 2); attr(x, "dim") <- NULL; print(attributes(x))
x <- structure(1:2, names = c("a", "b"), tag = "t"); dim(x) <- NULL; print(attributes(x))
x <- matrix(1:4, 2, dimnames = list(c("r", "s"), NULL)); dim(x) <- NULL; print(attributes(x))
x <- matrix(1:4, 2, dimnames = list(c("r", "s"), NULL)); attr(x, "dim") <- 4L; print(attributes(x))
x <- matrix(1:4, 2, dimnames = list(c("r", "s"), NULL)); dim(x) <- c(1, 4); print(attributes(x))
x <- 1:3; dim(x) <- NULL; print(attributes(x))
x <- pairlist(a = 1, 2); dim(x) <- NULL; print(x)
x <- list(a = 1, b = "q"); dim(x) <- NULL; str(x)
x <- list(a = 1, b = "q"); dim(x) <- 2; str(x)
r <- tryCatch({ x <- c(a = 1, b = 2); dim(x) <- 3; x }, error = function(e) conditionMessage(e)); str(r)
f <- function(x) { dim(x) <- NULL; x }
for (i in 1:3) print(f(c(u = 1i, v = 2i)))
