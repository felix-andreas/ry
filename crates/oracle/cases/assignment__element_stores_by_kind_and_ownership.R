# Tier 0 stores one element (`x[i] <- v`, `x[[i]] <- v`, `x$name <- v`, `m[i, j] <- v`) without
# the replacement call when the store changes no type and no attribute. Every vector type goes
# through it, at lengths on both sides of 64 (where a copy that only shares data has no storage
# of its own), in a function called three times (a slot frame, compiled on the second call) and
# at top level (a cell frame), with the original value kept by the caller.
show <- function(...) cat(..., "\n")
for (n in c(3L, 63L, 64L, 65L, 200L)) {
  f <- function(v, value) { v[1] <- value; v[2] <- value; v[length(v)] <- value; v }
  g <- function(v, value) { v[[1]] <- value; v[[length(v) + 1]] <- value; v }
  h <- function(v, value) { v[["a"]] <- value; v[["b"]] <- value; v$c <- value; v }
  for (seed in list(0, 0L, FALSE, "", list(NULL))) {
    v <- rep(seed, n)
    value <- if (is.list(seed)) "x" else switch(typeof(seed), character = "x", logical = TRUE, integer = 1L, 1)
    for (call in 1:3) {
      r <- f(v, value); a <- g(v, value)
      stopifnot(identical(v, rep(seed, n)))
    }
    show(n, typeof(r), length(r), sum(unlist(r) == value), length(a), identical(a[[length(a)]], value))
    if (is.list(seed)) { for (call in 1:3) b <- h(v, value); show(names(b)[n + 1:3], length(b)) }
  }
  # conversions
  d <- numeric(n); for (call in 1:3) { r <- f(d, 2L); r2 <- f(d, TRUE); r3 <- f(d, NA) ; r4 <- f(d, NA_integer_)}
  show(typeof(r), r[1], typeof(r2), r2[1], r3[1], r4[2])
  i <- integer(n); for (call in 1:3) { r <- f(i, TRUE); r2 <- f(i, 2.5); r3 <- f(i, "a"); r4 <- f(i, NA) }
  show(typeof(r), r[1], typeof(r2), r2[1], typeof(r3), r3[1], r4[1])
  # names
  nv <- setNames(numeric(n), paste0("k", seq_len(n)))
  k <- function(v) { v["k1"] <- 5; v[["k2"]] <- 6; v["new"] <- 7; v }
  for (call in 1:3) r <- k(nv)
  show(r[1], r[2], names(r)[length(r)], length(r), sum(nv))
  # top-level (cell frame)
  x <- numeric(n); y <- x
  x[1] <- 1; x[n] <- 2; x[n + 1] <- 3
  show(length(x), sum(x), sum(y))
  l <- vector("list", n); l2 <- l
  l[[1]] <- "a"; l$z <- 1; l[["z"]] <- 2; l[[n + 2]] <- 3
  show(length(l), names(l)[n + 1], l$z, length(l2))
  # matrices
  m <- matrix(0, n, 2); m2 <- m
  m[1, 1] <- 1; m[n, 2] <- 2
  show(sum(m), sum(m2), dim(m))
  mf <- function(m) { m[1, 1] <- 1L; m[nrow(m), 2] <- 2; m[1L, 2L] <- TRUE; m }
  for (call in 1:3) { r <- mf(m2); ri <- mf(matrix(0L, n, 2)); rc <- mf(matrix("", n, 2)); rl <- mf(matrix(list(), n, 2)) }
  show(sum(r), sum(m2), typeof(ri), sum(ri), typeof(rc), rc[1, 1], typeof(rl), dim(rl))
  # out of bounds and odd subscripts
  e <- function(m, i, j) tryCatch({ m[i, j] <- 9; sum(m) }, error = function(e) conditionMessage(e))
  show(e(m2, n + 1, 1), e(m2, 0, 1), e(m2, -1, 1), e(m2, NA, 1), e(m2, 1.9, 2.2), e(m2, TRUE, 1))
  # dimnames kept
  dm <- matrix(0, n, 2, dimnames = list(NULL, c("a", "b"))); dmf <- function(m) { m[2, 1] <- 3; m[3, 2] <- 4; m }
  for (call in 1:3) r <- dmf(dm)
  show(colnames(r), sum(r), sum(dm))
}
# self store, shared, attributes, objects
l <- list(1, 2); l[[2]] <- l; str(l)
v <- structure(1:3, class = "foo"); `[<-.foo` <- function(x, i, value) { cat("method\n"); unclass(x) }
v[1] <- 2L; print(v)
a <- array(0, c(2, 2, 2)); a[1] <- 1; a[[8]] <- 2; print(sum(a)); print(dim(a))
f <- factor(c("a", "b")); f[1] <- "b"; print(f)
d <- 1:10; d[3] <- 100L; print(d)   # ALTREP compact sequence target
d2 <- 1:100; d2[3] <- 0.5; print(sum(d2))
s <- c(a = "x"); s["b"] <- "y"; s[["a"]] <- "z"; print(s)
e <- new.env(); e$x <- 1; e[["y"]] <- 2; print(sort(ls(e)))
p <- pairlist(a = 1); p$b <- 2; print(names(p))
n <- NULL; n$a <- 1; n[["b"]] <- 2; str(n)
n2 <- NULL; n2[3] <- 1; print(n2)
q <- 1:3; q[[5]] <- 9L; print(q)
z <- c(1, 2); z[2.7] <- 9; print(z); z[0] <- 5; print(z); z[-1] <- 7; print(z)
u <- c("ä" = 1, b = 2); u["ä"] <- 3; u[enc2native("ä")] <- 4; print(u)
